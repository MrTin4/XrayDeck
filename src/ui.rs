use anyhow::Result;
use futures::StreamExt;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{
    assets::Assets,
    component::{
        button::*,
        input::{Input, InputState},
        *,
    },
    *,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use xray_deck::{
    api::{self, ApiSnapshot, NodeSort},
    refresh::RefreshSchedule,
    service::{self, ServiceInfo},
    settings::{Settings, data_dir, parse_refresh_interval},
    update::{self, Release},
    version::parse_version,
};

enum JobResult {
    Snapshot(ApiSnapshot),
    Release(Release),
    Upgraded(ApiSnapshot, Box<ServiceInfo>),
}
enum JobEvent {
    Progress(String),
    Finished(Box<Result<JobResult>>),
}

#[derive(Clone, Copy)]
enum JobArea {
    Nodes,
    Update,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Settings,
}

struct Deck {
    instance: crate::single_instance::Instance,
    page: Page,
    node_sort: NodeSort,
    file_version: Option<String>,
    local_loading: bool,
    starting: bool,
    startup_update_pending: bool,
    local_note: String,
    api: Entity<InputState>,
    service: Entity<InputState>,
    config: Entity<InputState>,
    core: Entity<InputState>,
    refresh_interval: Entity<InputState>,
    auto_refresh_draft: bool,
    refresh_schedule: RefreshSchedule,
    settings: Settings,
    settings_path: PathBuf,
    snapshot: Option<ApiSnapshot>,
    node_list: Entity<NodeList>,
    association: Option<ServiceInfo>,
    release: Option<Release>,
    nodes_status: String,
    update_status: String,
    settings_status: String,
    busy: bool,
    refreshing: bool,
    upgrading: Arc<AtomicBool>,
    close_requested: Arc<AtomicBool>,
    confirm_upgrade: bool,
}

impl Deck {
    fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        settings_path: PathBuf,
        settings: Settings,
        error: Option<String>,
        mut instance: crate::single_instance::Instance,
    ) -> Self {
        fn input(
            value: String,
            placeholder: &'static str,
            window: &mut Window,
            cx: &mut Context<Deck>,
        ) -> Entity<InputState> {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        }
        instance.attach(window).expect("无法登记 XrayDeck 单例窗口");
        let upgrading = Arc::new(AtomicBool::new(false));
        let close_requested = Arc::new(AtomicBool::new(false));
        let tray = match crate::tray::Tray::new(window, close_requested.clone()) {
            Ok(tray) => Some(std::rc::Rc::new(tray)),
            Err(error) => {
                xray_deck::logging::event(&format!("托盘初始化失败：{error:#}"));
                None
            }
        };
        if let Some(tray) = tray.clone() {
            cx.on_app_quit(move |_, _| {
                tray.remove_icon();
                async {}
            })
            .detach();
        }
        let guard = upgrading.clone();
        let requested = close_requested.clone();
        window.on_window_should_close(cx, move |_, cx| {
            if let Some(tray) = &tray {
                tray.hide();
                false
            } else if guard.load(Ordering::SeqCst) {
                requested.store(true, Ordering::SeqCst);
                false
            } else {
                cx.quit();
                true
            }
        });
        let settings_loaded = error.is_none();
        let mut deck = Self {
            instance,
            page: Page::Home,
            node_sort: NodeSort::Delay,
            file_version: None,
            local_loading: false,
            starting: settings_loaded,
            startup_update_pending: true,
            local_note: String::new(),
            api: input(settings.api_address.clone(), "127.0.0.1:8080", window, cx),
            service: input(settings.service_name.clone(), "例如 winsw-xray", window, cx),
            config: input(
                settings.winsw_config.display().to_string(),
                "WinSW XML 完整路径",
                window,
                cx,
            ),
            core: input(
                settings.xray_path.display().to_string(),
                "xray.exe 完整路径",
                window,
                cx,
            ),
            refresh_interval: input(
                settings.refresh_interval_secs.to_string(),
                "刷新间隔（秒）",
                window,
                cx,
            ),
            auto_refresh_draft: settings.auto_refresh,
            refresh_schedule: RefreshSchedule::default(),
            settings,
            settings_path,
            snapshot: None,
            node_list: cx.new(|_| NodeList::default()),
            association: None,
            release: None,
            settings_status: error.clone().unwrap_or_default(),
            update_status: String::new(),
            nodes_status: error
                .unwrap_or_else(|| "启动忙碌：正在读取核心版本并校验服务关联…".into()),
            busy: false,
            refreshing: false,
            upgrading,
            close_requested,
            confirm_upgrade: false,
        };
        if settings_loaded {
            deck.refresh_schedule.configure(
                deck.settings.auto_refresh,
                deck.settings.refresh_interval_secs,
                Instant::now(),
            );
            deck.inspect_local(cx);
        } else {
            deck.check(cx);
        }
        let executor = cx.background_executor().clone();
        cx.spawn(async move |entity, cx| {
            loop {
                executor.timer(Duration::from_secs(1)).await;
                if entity
                    .update(cx, |this, cx| {
                        this.instance.poll_activation();
                        if this.close_requested.load(Ordering::SeqCst)
                            && !this.upgrading.load(Ordering::SeqCst)
                        {
                            cx.quit();
                            return;
                        }
                        let blocked = this.busy
                            || this.starting
                            || this.local_loading
                            || this.confirm_upgrade
                            || this.page != Page::Home
                            || this.close_requested.load(Ordering::SeqCst);
                        if this.refresh_schedule.poll(Instant::now(), blocked) {
                            this.refresh(cx);
                        }
                        if this.settings.auto_refresh && this.page == Page::Home {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        deck
    }

    fn inspect_local(&mut self, cx: &mut Context<Self>) {
        if self.local_loading {
            return;
        }
        self.association = None;
        self.file_version = None;
        if self.settings.xray_path.as_os_str().is_empty() {
            self.local_note = "请在设置中配置 xray.exe 路径".into();
            self.finish_startup(cx);
            return;
        }
        self.local_loading = true;
        self.local_note = "正在读取核心文件版本并校验服务关联…".into();
        let settings = self.settings.clone();
        let (sender, receiver) = futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let version = service::file_version(&settings.xray_path).map_err(|e| format!("{e:#}"));
            let association = service::validate(&settings).map_err(|e| format!("{e:#}"));
            let _ = sender.send((version, association));
        });
        cx.spawn(async move |entity, cx| {
            let result = receiver.await;
            let _ = entity.update(cx, |this, cx| {
                this.local_loading = false;
                match result {
                    Ok((version, association)) => {
                        let version_note = match version {
                            Ok(version) => {
                                this.file_version = Some(version);
                                String::new()
                            }
                            Err(error) => format!("文件版本读取失败：{error}；"),
                        };
                        this.local_note = match association {
                            Ok(info) => {
                                let note = if info.image_verified {
                                    "服务关联已校验"
                                } else {
                                    "服务归属已校验；实际进程路径在升级提权后复核"
                                };
                                this.association = Some(info);
                                format!("{version_note}{note}")
                            }
                            Err(error) => format!("{version_note}服务关联未通过：{error}"),
                        };
                    }
                    Err(_) => this.local_note = "本地核心检查异常结束，请重试校验".into(),
                }
                this.finish_startup(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_startup(&mut self, cx: &mut Context<Self>) {
        if !self.starting {
            return;
        }
        self.starting = false;
        self.nodes_status = "启动就绪 · 本地核心检查结果可在设置中查看".into();
        if self.settings.auto_refresh {
            self.refresh(cx);
        } else {
            self.nodes_status = "启动就绪 · 可点击刷新出站读取节点".into();
            self.check(cx);
        }
        cx.notify();
    }

    fn read_settings(&mut self, cx: &Context<Self>) -> Settings {
        Settings {
            api_address: self.api.read(cx).value().to_string().trim().into(),
            service_name: self.service.read(cx).value().to_string().trim().into(),
            winsw_config: PathBuf::from(self.config.read(cx).value().to_string().trim()),
            xray_path: PathBuf::from(self.core.read(cx).value().to_string().trim()),
            include_prerelease: self.settings.include_prerelease,
            auto_refresh: self.auto_refresh_draft,
            refresh_interval_secs: self.settings.refresh_interval_secs,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let mut settings = self.read_settings(cx);
        settings.refresh_interval_secs =
            match parse_refresh_interval(&self.refresh_interval.read(cx).value()) {
                Ok(seconds) => seconds,
                Err(error) => {
                    self.settings_status = format!("保存失败：{error:#}");
                    return false;
                }
            };
        match settings.save(&self.settings_path) {
            Ok(()) => {
                if self.settings.api_address != settings.api_address {
                    self.snapshot = None;
                    self.node_list.update(cx, |list, cx| {
                        *list = NodeList::default();
                        cx.notify();
                    });
                    self.nodes_status = "API 地址已变更，请刷新出站".into();
                }
                let local_changed = self.settings.api_address != settings.api_address
                    || self.settings.service_name != settings.service_name
                    || self.settings.winsw_config != settings.winsw_config
                    || self.settings.xray_path != settings.xray_path;
                if local_changed {
                    self.association = None;
                    self.release = None;
                    self.update_status.clear();
                }
                if self.settings.auto_refresh != settings.auto_refresh
                    || self.settings.refresh_interval_secs != settings.refresh_interval_secs
                    || self.settings.api_address != settings.api_address
                {
                    self.refresh_schedule.configure(
                        settings.auto_refresh,
                        settings.refresh_interval_secs,
                        Instant::now(),
                    );
                }
                self.settings = settings;
                self.confirm_upgrade = false;
                self.settings_status = "设置已保存".into();
                if local_changed || self.file_version.is_none() {
                    self.inspect_local(cx);
                }
                true
            }
            Err(e) => {
                self.settings_status = format!("保存失败：{e:#}");
                false
            }
        }
    }

    fn set_job_status(&mut self, area: JobArea, message: String) {
        match area {
            JobArea::Nodes => self.nodes_status = message,
            JobArea::Update => self.update_status = message,
        }
    }

    fn job(
        &mut self,
        area: JobArea,
        label: &str,
        work: impl FnOnce(&mut dyn FnMut(&str)) -> Result<JobResult> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        if !self.refreshing || self.snapshot.is_none() {
            self.set_job_status(area, label.into());
        }
        cx.notify();
        let (sender, mut receiver) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            let mut progress = |text: &str| {
                let _ = sender.unbounded_send(JobEvent::Progress(text.into()));
            };
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&mut progress)))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("后台任务异常结束")));
            let _ = sender.unbounded_send(JobEvent::Finished(Box::new(result)));
        });
        cx.spawn(async move |entity, cx| {
            while let Some(event) = receiver.next().await {
                let result = match event {
                    JobEvent::Progress(message) => {
                        let _ = entity.update(cx, |this, cx| {
                            this.set_job_status(area, message);
                            cx.notify()
                        });
                        continue;
                    }
                    JobEvent::Finished(result) => *result,
                };
                let _ = entity.update(cx, |this, cx| {
                    this.busy = false;
                    this.refreshing = false;
                    this.upgrading.store(false, Ordering::SeqCst);
                    let failed = result.is_err();
                    match result {
                        Ok(JobResult::Snapshot(snapshot)) => {
                            this.nodes_status = "已连接".into();
                            this.set_snapshot(snapshot, cx)
                        }
                        Ok(JobResult::Release(release)) => {
                            this.update_status.clear();
                            this.release = Some(release);
                        }
                        Ok(JobResult::Upgraded(snapshot, info)) => {
                            this.update_status = "升级成功，已重新读取 API 能力、节点和版本".into();
                            this.nodes_status = "已连接 · 升级后已刷新".into();
                            this.set_snapshot(snapshot, cx);
                            this.file_version = Some(info.file_version.clone());
                            this.association = Some(*info);
                            this.release = None;
                            this.confirm_upgrade = false
                        }
                        Err(e) => this.set_job_status(area, format!("操作失败：{e:#}")),
                    }
                    xray_deck::logging::event(if failed {
                        "后台操作失败，详细原因显示在界面"
                    } else {
                        "后台操作完成"
                    });
                    cx.notify();
                    if this.close_requested.load(Ordering::SeqCst) {
                        cx.quit()
                    } else if this.startup_update_pending {
                        // The initial node refresh may fail; still check once.
                        this.check(cx);
                    }
                });
                break;
            }
        })
        .detach();
    }

    fn set_snapshot(&mut self, snapshot: ApiSnapshot, cx: &mut Context<Self>) {
        self.node_list.update(cx, |list, cx| {
            if list.update_snapshot(&snapshot, self.node_sort) {
                cx.notify();
            }
        });
        self.snapshot = Some(snapshot);
    }

    fn update_node_sort(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = &self.snapshot {
            self.node_list.update(cx, |list, cx| {
                if list.update_snapshot(snapshot, self.node_sort) {
                    cx.notify();
                }
            });
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.refresh_schedule.restart(Instant::now());
        self.refreshing = true;
        let address = self.settings.api_address.clone();
        self.job(
            JobArea::Nodes,
            "正在连接并读取 API…",
            move |_| Ok(JobResult::Snapshot(api::inspect_blocking(&address)?)),
            cx,
        );
    }
    fn validate(&mut self, cx: &mut Context<Self>) {
        if !self.save(cx) {
            cx.notify();
            return;
        }
        self.inspect_local(cx);
        cx.notify();
    }
    fn check(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        // Consume before dispatch so later jobs/local validation never repeat it.
        self.startup_update_pending = false;
        self.confirm_upgrade = false;
        self.release = None;
        let pre = self.settings.include_prerelease;
        self.job(
            JobArea::Update,
            "正在检查官方更新…",
            move |_| Ok(JobResult::Release(update::check(pre)?)),
            cx,
        );
    }
    fn upgrade(&mut self, cx: &mut Context<Self>) {
        // Always re-read current inputs; a stale successful validation is insufficient.
        let settings = self.read_settings(cx);
        let Some(release) = self.release.clone() else {
            return;
        };
        let critical_stage = self.upgrading.clone();
        self.confirm_upgrade = false;
        self.job(
            JobArea::Update,
            "正在下载和校验；随后请求 UAC、重启服务并验证，失败时自动回滚…",
            move |progress| {
                let plan = update::prepare_with_progress(&settings, &release, progress)?;
                critical_stage.store(true, Ordering::SeqCst);
                progress("下载校验完成，正在请求 UAC…");
                update::elevate_with_progress(&plan, progress)?;
                Ok(JobResult::Upgraded(
                    api::inspect_blocking(&settings.api_address)?,
                    Box::new(service::validate(&settings)?),
                ))
            },
            cx,
        );
    }

    fn newer(&self) -> bool {
        match (&self.release, &self.association) {
            (Some(r), Some(s)) => {
                matches!((parse_version(&r.tag_name),parse_version(&s.file_version)),(Ok(a),Ok(b)) if a>b)
            }
            _ => false,
        }
    }

    fn refresh_hint(&self) -> String {
        if self.starting {
            return "启动忙碌 · 等待本地检查完成".into();
        }
        if self.refreshing {
            return "正在刷新出站…".into();
        }
        if !self.settings.auto_refresh {
            return "自动刷新已关闭".into();
        }
        if self.confirm_upgrade {
            return "自动刷新已暂停 · 等待升级确认".into();
        }
        if self.busy || self.local_loading || self.close_requested.load(Ordering::SeqCst) {
            return "自动刷新已暂停 · 后台忙碌".into();
        }
        match self.refresh_schedule.remaining_seconds(Instant::now()) {
            Some(0) => "即将自动刷新…".into(),
            Some(seconds) => format!("下次自动刷新：{seconds} 秒"),
            None => "自动刷新未启动".into(),
        }
    }
}

fn label(text: impl Into<SharedString>) -> Div {
    div().text_sm().text_color(rgb(0x94a3b8)).child(text.into())
}

fn nodes_heading(summary: String, window: &mut Window) -> Div {
    let title: SharedString = "出站节点".into();
    let summary: SharedString = summary.into();
    let title_size = window.rem_size() * 1.125;
    let summary_size = window.rem_size() * 0.875;
    let title_height = window.pixel_snap(window.rem_size() * 1.75);
    let summary_height = window.pixel_snap(window.rem_size() * 1.25);
    let style = TextStyle {
        font_family: "Microsoft YaHei UI".into(),
        ..window.text_style()
    };
    let baseline = |text: &SharedString, size, height| {
        let line =
            window
                .text_system()
                .shape_line(text.clone(), size, &[style.to_run(text.len())], None);
        (height + line.ascent - line.descent) / 2.
    };
    let title_baseline = baseline(&title, title_size, title_height);
    let summary_baseline = baseline(&summary, summary_size, summary_height);
    let baseline = title_baseline.max(summary_baseline);
    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .flex_shrink_0()
                .whitespace_nowrap()
                .pt(baseline - title_baseline)
                .text_size(title_size)
                .line_height(title_height)
                .child(title),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .pt(baseline - summary_baseline)
                .text_size(summary_size)
                .line_height(summary_height)
                .text_color(rgb(0x94a3b8))
                .child(summary),
        )
}

fn refresh_button(deck: &Deck, busy: bool, cx: &mut Context<Deck>) -> Button {
    let seconds = deck.refresh_schedule.remaining_seconds(Instant::now());
    // Polling must not hide/reinsert content or change the button's dimensions.
    let countdown_visible =
        deck.settings.auto_refresh && (!busy || deck.refreshing) && !deck.confirm_upgrade;
    deck_button("refresh", cx)
        .label("刷新出站")
        .disabled(busy && !deck.refreshing)
        .tooltip(deck.refresh_hint())
        .when(deck.settings.auto_refresh, |button| {
            button.child(
                // Two fixed-width monospace digits; the separate '+' slot keeps
                // the unit stationary even for configured intervals over 99 sec.
                div()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .when(!countdown_visible || seconds.is_none(), |view| {
                        view.invisible()
                    })
                    .child(
                        div()
                            .w(px(18.))
                            .text_right()
                            .font_family("Consolas")
                            .child(seconds.unwrap_or(0).min(99).to_string()),
                    )
                    .child(div().w(px(8.)).child(if seconds.is_some_and(|s| s > 99) {
                        "+"
                    } else {
                        ""
                    }))
                    .child("秒"),
            )
        })
        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
}

/// GPUI's measured text leaves expose size, but no typographic baseline to Taffy.
/// Align these two single-line blocks using the same shaped metrics as text painting.
fn app_heading(
    runtime_version: &str,
    update_summary: String,
    busy: bool,
    window: &mut Window,
    cx: &mut Context<Deck>,
) -> Div {
    let title: SharedString = "XrayDeck".into();
    let version: SharedString = format!("运行版本 {runtime_version}").into();
    let title_size = window.rem_size() * 1.5;
    let version_size = window.rem_size() * 0.875;
    let title_height = window.pixel_snap(window.rem_size() * 2.);
    let version_height = window.pixel_snap(window.rem_size() * 1.25);
    let text_style = TextStyle {
        font_family: "Microsoft YaHei UI".into(),
        ..window.text_style()
    };
    let title_style = TextStyle {
        font_weight: FontWeight::BOLD,
        ..text_style.clone()
    };
    let title_line = window.text_system().shape_line(
        title.clone(),
        title_size,
        &[title_style.to_run(title.len())],
        None,
    );
    let version_line = window.text_system().shape_line(
        version.clone(),
        version_size,
        &[text_style.to_run(version.len())],
        None,
    );
    // Text paint uses (line_height - ascent - descent) / 2 + ascent.
    let title_baseline = (title_height + title_line.ascent - title_line.descent) / 2.;
    let version_baseline = (version_height + version_line.ascent - version_line.descent) / 2.;
    let baseline = title_baseline.max(version_baseline);

    let title_and_version = div()
        .flex()
        .items_start()
        .flex_shrink_0()
        .gap_3()
        .font_family(text_style.font_family)
        .child(
            div()
                .pt(baseline - title_baseline)
                .text_size(title_size)
                .line_height(title_height)
                .font_weight(FontWeight::BOLD)
                .whitespace_nowrap()
                .child(title),
        )
        .child(
            div()
                .pt(baseline - version_baseline)
                .text_size(version_size)
                .line_height(version_height)
                .text_color(rgb(0x94a3b8))
                .whitespace_nowrap()
                .child(version),
        );

    div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_wrap()
        .items_start()
        .gap_3()
        .child(title_and_version)
        .child(
            div()
                .min_w(px(0.))
                .pt(baseline - version_baseline)
                .text_size(version_size)
                .line_height(version_height)
                .child(update_summary),
        )
        .child(
            update_link("home-check", busy)
                .mt(baseline - version_baseline)
                .h(version_height)
                .line_height(version_height)
                .label("检查更新")
                .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
        )
}

fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_5()
        .rounded_lg()
        .bg(rgb(0x172033))
        .border_1()
        .border_color(rgb(0x29354a))
}

const NODE_COLUMN_WIDTHS: [Option<f32>; 6] = [
    None,
    Some(90.),
    Some(190.),
    Some(85.),
    Some(130.),
    Some(200.),
];
const NODE_SCROLL_GUTTER: f32 = 12.;
const NODE_HEADER_TEXT: u32 = 0x94a3b8;
const NODE_HEADER_BACKGROUND: u32 = 0x1e293b;
const BUTTON_HOVER_BACKGROUND: u32 = 0x29384f;
const BUTTON_ACTIVE_BACKGROUND: u32 = 0x334155;
const LINK_TEXT: u32 = 0x93c5fd;
const LINK_HOVER_TEXT: u32 = 0xf8fafc;
const LINK_ACTIVE_TEXT: u32 = 0xe0f2fe;

/// All action buttons share the table header palette and interaction states.
fn deck_button(id: &'static str, cx: &App) -> Button {
    Button::new(id).custom(
        ButtonCustomVariant::new(cx)
            .foreground(rgb(NODE_HEADER_TEXT).into())
            .color(rgb(NODE_HEADER_BACKGROUND).into())
            .hover(rgb(BUTTON_HOVER_BACKGROUND).into())
            .active(rgb(BUTTON_ACTIVE_BACKGROUND).into()),
    )
}

/// Update checks are inline links; other actions use the shared filled style.
fn update_link(id: &'static str, disabled: bool) -> Button {
    Button::new(id)
        .link()
        .small()
        .px_0()
        .disabled(disabled)
        .when(disabled, |button| button.text_color(rgb(0x64748b)))
}

fn deck_input(state: &Entity<InputState>, disabled: bool) -> Input {
    Input::new(state)
        .disabled(disabled)
        .bg(rgb(if disabled { 0x172033 } else { 0x101827 }))
        .border_1()
        .border_color(rgb(0x29354a))
}

fn format_bytes(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "—".into();
    };
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit < units.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// Both header and body use the same column widths, shrink rules and padding.
fn node_columns(values: [String; 6], header_sort: Option<NodeSort>, cx: &mut Context<Deck>) -> Div {
    let header = header_sort.is_some();
    let mut row = div()
        .w_full()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .text_sm();
    for (index, (width, value)) in NODE_COLUMN_WIDTHS.into_iter().zip(values).enumerate() {
        let cell = div().min_w(px(0.)).truncate();
        let cell = if let Some(sort) = header_sort.filter(|_| matches!(index, 3 | 4)) {
            let arrow = match (index, sort) {
                (3, NodeSort::Delay) | (4, NodeSort::Oldest) => " ↑",
                (3, NodeSort::DelayDescending) | (4, NodeSort::Newest) => " ↓",
                _ => " ↕",
            };
            cell.child(
                deck_button(
                    if index == 3 {
                        "sort-delay"
                    } else {
                        "sort-time"
                    },
                    cx,
                )
                .small()
                .w_full()
                .px_0()
                .label(format!("{value}{arrow}"))
                .tooltip(if index == 3 {
                    "选中节点优先；组内切换延迟升序 / 降序，无有效延迟置后"
                } else {
                    "选中节点优先；组内切换时间升序 / 降序，无有效时间置后"
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.node_sort = if index == 3 {
                        this.node_sort.toggle_delay()
                    } else {
                        this.node_sort.toggle_time()
                    };
                    this.update_node_sort(cx);
                    cx.notify();
                })),
            )
        } else {
            cell.child(value)
        };
        let cell = match width {
            Some(width) => cell.w(px(width)).flex_shrink_0(),
            None => cell.flex_1(),
        };
        row = row.child(cell.when(header || matches!(index, 1 | 3 | 4 | 5), |cell| {
            cell.text_center()
        }));
    }
    if header {
        row.rounded_md()
            .bg(rgb(NODE_HEADER_BACKGROUND))
            .text_color(rgb(NODE_HEADER_TEXT))
            .font_weight(FontWeight::MEDIUM)
    } else {
        row.rounded_md().bg(rgb(0x101827))
    }
}

/// Keep the scrollable subtree cached across status/countdown updates. Only
/// changed visible rows invalidate it; checked_at alone never repaints the list.
#[derive(Default)]
struct NodeList {
    rows: Vec<[String; 6]>,
    note: Option<String>,
    selected: std::collections::BTreeSet<String>,
}

impl NodeList {
    fn update_snapshot(&mut self, snapshot: &ApiSnapshot, sort: NodeSort) -> bool {
        let note = snapshot
            .nodes_note
            .as_ref()
            .map(|note| format!("节点读取：{note}"))
            .or_else(|| snapshot.nodes.is_empty().then(|| "核心未返回出站".into()));
        let rows = snapshot
            .sorted_nodes(sort)
            .into_iter()
            .map(|(node, observation)| {
                let delay = match observation {
                    Some(o) if !o.alive => "不可达".to_string(),
                    Some(o) => o
                        .delay_ms
                        .filter(|&v| v >= 0)
                        .map(|v| format!("{v} ms"))
                        .unwrap_or_else(|| "暂无数据".into()),
                    None => "暂无数据".into(),
                };
                let time = observation
                    .filter(|o| o.last_try > 0)
                    .and_then(|o| chrono::DateTime::from_timestamp(o.last_try, 0))
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%m-%d %H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_else(|| {
                        if observation.is_some() {
                            "核心未提供"
                        } else {
                            "—"
                        }
                        .into()
                    });

                [
                    node.tag.clone(),
                    format!(
                        "{}{}",
                        node.protocol,
                        if node.proxy { "" } else { " · 系统" }
                    ),
                    match (&node.address, node.port) {
                        (Some(a), Some(p)) => format!("{a}:{p}"),
                        _ => "地址未知 / 不适用".into(),
                    },
                    delay,
                    time,
                    {
                        let traffic = snapshot.traffic.get(&node.tag);
                        format!(
                            "↑ {} / ↓ {}",
                            format_bytes(traffic.and_then(|t| t.uplink)),
                            format_bytes(traffic.and_then(|t| t.downlink))
                        )
                    },
                ]
            })
            .collect::<Vec<_>>();
        if self.rows != rows || self.note != note || self.selected != snapshot.selected_nodes {
            self.rows = rows;
            self.note = note;
            self.selected = snapshot.selected_nodes.clone();
            return true;
        }
        false
    }
}

impl Render for NodeList {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("nodes")
            .size_full()
            .overflow_y_scroll()
            .scrollbar_width(px(0.))
            .pr(px(NODE_SCROLL_GUTTER))
            .flex()
            .flex_col()
            .gap_2()
            .when_some(self.note.clone(), |list, note| list.child(label(note)))
            .when(self.rows.is_empty() && self.note.is_none(), |list| {
                list.child(label("连接后显示运行时节点，节点凭据不会展示或写入日志。"))
            })
            .children(self.rows.iter().map(|values| {
                div()
                    .id(SharedString::from(values[0].clone()))
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .rounded_md()
                    .bg(rgb(if self.selected.contains(&values[0]) {
                        0x163347
                    } else {
                        0x101827
                    }))
                    .children(NODE_COLUMN_WIDTHS.into_iter().zip(values).enumerate().map(
                        |(index, (width, value))| {
                            let cell = div().min_w(px(0.));
                            let cell = if index == 0 && self.selected.contains(value) {
                                cell.truncate()
                                    .text_color(rgb(0x67e8f9))
                                    .child(format!("● 已选中 · {value}"))
                            } else {
                                cell.truncate().child(value.clone())
                            };
                            let cell = match width {
                                Some(width) => cell.w(px(width)).flex_shrink_0(),
                                None => cell.flex_1(),
                            };
                            cell.when(matches!(index, 1 | 3 | 4 | 5), |cell| cell.text_center())
                        },
                    ))
            }))
    }
}

impl Render for Deck {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy || self.local_loading || self.starting;
        let newer = self.newer();
        let runtime_version = self
            .association
            .as_ref()
            .and_then(|info| info.runtime_version.clone())
            .unwrap_or_else(|| {
                if self.local_loading {
                    "读取中…"
                } else {
                    "未知"
                }
                .into()
            });
        let mut services = div().flex().flex_col().gap_1();
        if let Some(snapshot) = &self.snapshot {
            if let Some(note) = &snapshot.discovery_note {
                services = services.child(label(note.clone()));
            }
            for name in &snapshot.services {
                services = services.child(label(name.clone()));
            }
        }
        let update_text = self
            .release
            .as_ref()
            .map(|r| {
                format!(
                    "{} · {} · {} · {}",
                    match self
                        .file_version
                        .as_ref()
                        .and_then(|version| parse_version(version).ok())
                    {
                        Some(current) => match parse_version(&r.tag_name) {
                            Ok(latest) if latest > current => "有更新",
                            Ok(_) => "核心已是最新或更新版本",
                            Err(_) => "发布版本无法比较",
                        },
                        None => "版本未知，暂无法比较",
                    },
                    r.tag_name,
                    if r.prerelease {
                        "预发布"
                    } else {
                        "正式版"
                    },
                    r.published_at.as_deref().unwrap_or("发布时间未知")
                )
            })
            .unwrap_or_else(|| "尚未检查更新".into());
        let home = self.page == Page::Home;
        let observation_note = self.snapshot.as_ref().and_then(|snapshot| {
            let notes: Vec<_> = [
                &snapshot.observations_note,
                &snapshot.traffic_note,
                &snapshot.balancers_note,
            ]
            .into_iter()
            .filter_map(|note| note.as_deref())
            .collect();
            (!notes.is_empty()).then(|| notes.join("；"))
        });
        let update_summary = if self.update_status.is_empty() {
            update_text
                .split(" · ")
                .take(2)
                .collect::<Vec<_>>()
                .join(" · ")
        } else {
            self.update_status.clone()
        };
        let nodes_summary = self
            .snapshot
            .as_ref()
            .map(|snapshot| {
                format!(
                    "{} · {} 个出站 · 最近读取：{}",
                    self.nodes_status,
                    snapshot.nodes.len(),
                    snapshot.checked_at
                )
            })
            .unwrap_or_else(|| self.nodes_status.clone());
        div().size_full().bg(rgb(0x0b1220)).text_color(rgb(0xe2e8f0)).font_family("Microsoft YaHei UI")
            .child(div().id("page").size_full().p_4().flex().flex_col().gap_3()
                .when(!home, |view| view.overflow_y_scroll())
                .child(div().flex().justify_between().items_center().gap_3().flex_shrink_0()
                    .child(app_heading(&runtime_version, update_summary, busy && !self.refreshing, window, cx))
                    .child(div().flex().gap_3().items_center().flex_shrink_0()
                        .child(label(concat!("v", env!("CARGO_PKG_VERSION"))))
                        .child(deck_button("navigation", cx).label(if home { "设置" } else { "返回主页" }).on_click(cx.listener(|this,_,_,cx| {
                            this.page = if this.page == Page::Home { Page::Settings } else { Page::Home };
                            cx.notify();
                        })))))
                .when(home, |view| view
                .child(card().flex_1().min_h(px(0.)).p_3().gap_2().child(div().flex().justify_between().items_center()
                    .child(nodes_heading(nodes_summary, window))
                    .child(div().flex_shrink_0().child(refresh_button(self, busy, cx))))
                    .child(div().flex_shrink_0().pr(px(NODE_SCROLL_GUTTER)).child(node_columns(
                        ["节点名称", "协议", "服务器", "观测延迟", "观测时间", "累计流量 ↑ / ↓"].map(String::from), Some(self.node_sort), cx)))
                    .child(div().flex_1().min_h(px(0.)).child(self.node_list.clone().cached(StyleRefinement::default().size_full())))
                    .when_some(observation_note, |view, note| view.child(div().flex_shrink_0()
                        .border_t_1().border_color(rgb(0x29354a)).pt_2()
                        .child(label(note))))))
                .when(!home, |view| view
                .child(card().child(div().text_lg().child("连接配置与 API 诊断"))
                    .child(label(self.nodes_status.clone()))
                    .when(!self.settings_status.is_empty(), |view| view.child(label(self.settings_status.clone())))
                    .child(label("API 地址（保存后生效）"))
                    .child(div().flex().gap_3().items_center().child(div().flex_1().child(deck_input(&self.api, busy)))
                        .child(deck_button("connect", cx).label("保存并测试连接").disabled(busy).on_click(cx.listener(|this,_,_,cx| {
                            if this.save(cx) { this.refresh(cx); }
                            cx.notify();
                        }))))
                    .child(services))
                .child(card().child(div().text_lg().child("出站自动刷新"))
                    .child(div().flex().gap_3().items_center()
                        .child(deck_button("auto-refresh", cx).label(if self.auto_refresh_draft { "自动刷新：开启" } else { "自动刷新：关闭" }).disabled(busy).on_click(cx.listener(|this,_,_,cx| { this.auto_refresh_draft = !this.auto_refresh_draft; cx.notify(); })))
                        .child(div().w(px(180.)).child(deck_input(&self.refresh_interval, busy)))
                        .child(label("秒（5–86400）"))
                        .child(deck_button("save-refresh", cx).label("保存刷新设置").disabled(busy).on_click(cx.listener(|this,_,_,cx| { this.save(cx); cx.notify(); }))))
                    .when(!self.settings_status.is_empty(), |view| view.child(label(self.settings_status.clone())))
                    .child(label("默认关闭，间隔默认 30 秒。保存后生效；设置页及后台忙碌时跳过刷新。刷新只读取核心已有数据，不触发测速。")))
                .child(card().child(div().text_lg().child("本地服务关联"))
                    .child(label("Windows 服务名")).child(deck_input(&self.service, busy))
                    .child(label("WinSW 配置 XML")).child(deck_input(&self.config, busy))
                    .child(label("xray.exe 完整路径")).child(deck_input(&self.core, busy))
                    .child(div().flex().gap_3().items_center()
                        .child(deck_button("save", cx).label("保存设置").disabled(busy).on_click(cx.listener(|this,_,_,cx|{this.save(cx);cx.notify()})))
                        .child(deck_button("validate", cx).label("校验关联与版本").disabled(busy).on_click(cx.listener(|this,_,_,cx|this.validate(cx)))))
                    .child(label(self.local_note.clone()))
                    .when(!self.settings_status.is_empty(), |view| view.child(label(self.settings_status.clone())))
                    .child(label(self.association.as_ref().map(|s|format!("服务 {} · PID {} · 运行版本：{} · {}",s.name,s.process_id,
                        s.runtime_version.as_deref().unwrap_or("未知"),
                        [s.runtime_version_source.as_deref(), s.runtime_version_note.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" · ")))
                        .unwrap_or_else(||"校验服务、包装器、核心路径及 API 监听进程后才允许升级。".into()))))
                .child(card().child(div().text_lg().child("核心更新"))
                    .when(!self.update_status.is_empty(), |view| view.child(label(self.update_status.clone())))
                    .when(self.release.is_some() || self.update_status.is_empty(), |view| view.child(label(update_text)))
                    .child(div().flex().gap_3().items_center()
                        .child(deck_button("channel", cx).label(if self.settings.include_prerelease{"渠道：含预发布"}else{"渠道：仅正式版"}).disabled(busy).on_click(cx.listener(|this,_,_,cx|{this.settings.include_prerelease = !this.settings.include_prerelease;this.release=None;this.update_status.clear();this.save(cx);cx.notify()})))
                        .child(update_link("check", busy).label("检查官方更新").on_click(cx.listener(|this,_,_,cx|this.check(cx))))
                        .child(deck_button("upgrade", cx).label("升级核心…").disabled(busy||!newer).on_click(cx.listener(|this,_,_,cx|{this.confirm_upgrade=true;cx.notify()}))))
                    .when(self.confirm_upgrade,|view|view.child(div().flex().flex_col().gap_2()
                        .child("升级将请求 UAC，重启服务并中断现有连接。失败时尝试恢复备份。")
                        .child(div().flex().gap_2()
                            .child(deck_button("confirm", cx).label("确认升级并重启").disabled(busy).on_click(cx.listener(|this,_,_,cx|this.upgrade(cx))))
                            .child(deck_button("cancel", cx).label("返回").disabled(busy).on_click(cx.listener(|this,_,_,cx|{this.confirm_upgrade=false;cx.notify()}))))))
                    .child(label("使用系统网络下载。更新缓存与备份保存在 APPDATA\\XrayDeck。")))
                .child(label(format!("设置：{}",self.settings_path.display())))))
    }
}

pub fn run(instance: crate::single_instance::Instance) -> Result<()> {
    let directory = data_dir()?;
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("settings.json");
    let (settings, error) = match Settings::load(&path) {
        Ok(s) => (s, None),
        Err(e) => (
            Settings::default(),
            Some(format!("设置加载失败：{e:#}；保存将覆盖原文件")),
        ),
    };
    gpui_kit::application().with_assets(Assets).run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        let theme = Theme::global_mut(cx);
        theme.foreground = rgb(0xe2e8f0).into();
        theme.muted_foreground = rgb(0x94a3b8).into();
        theme.input = rgb(0x29354a).into();
        theme.border = rgb(0x29354a).into();
        theme.ring = rgb(0x93c5fd).into();
        theme.focus_ring = false;
        theme.caret = rgb(0xe2e8f0).into();
        theme.selection = rgb(0x334155).into();
        theme.link = rgb(LINK_TEXT).into();
        theme.link_hover = rgb(LINK_HOVER_TEXT).into();
        theme.link_active = rgb(LINK_ACTIVE_TEXT).into();
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("XrayDeck".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(size(px(1120.), px(920.)), cx)),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Deck::new(window, cx, path, settings, error, instance))
        })
        .expect("无法打开 XrayDeck 窗口");
        cx.activate(true);
    });
    Ok(())
}

#[cfg(test)]
mod node_list_tests {
    use super::{ApiSnapshot, NodeList, NodeSort, api};

    fn snapshot() -> ApiSnapshot {
        ApiSnapshot {
            services: vec![],
            discovery_note: None,
            nodes: vec![api::Node {
                tag: "proxy".into(),
                protocol: "vless".into(),
                address: Some("example.com".into()),
                port: Some(443),
                proxy: true,
            }],
            nodes_note: None,
            observations: vec![],
            observations_note: None,
            checked_at: "before".into(),
            ..Default::default()
        }
    }

    #[test]
    fn unchanged_rows_do_not_invalidate_the_list_on_refresh() {
        let mut list = NodeList::default();
        let mut snapshot = snapshot();
        assert!(list.update_snapshot(&snapshot, NodeSort::Delay));
        snapshot.checked_at = "after".into();
        assert!(!list.update_snapshot(&snapshot, NodeSort::Delay));
        snapshot.observations.push(api::Observation {
            tag: "proxy".into(),
            alive: true,
            delay_ms: Some(42),
            last_try: 0,
            burst: false,
        });
        assert!(list.update_snapshot(&snapshot, NodeSort::Delay));
        assert_eq!(list.rows[0][3], "42 ms");
        assert!(!list.update_snapshot(&snapshot, NodeSort::Delay));
    }

    #[test]
    fn sorting_and_removing_nodes_update_the_visible_rows() {
        let mut list = NodeList::default();
        let mut snapshot = snapshot();
        let mut second = snapshot.nodes[0].clone();
        second.tag = "second".into();
        snapshot.nodes.push(second);
        snapshot.observations = vec![api::Observation {
            tag: "second".into(),
            alive: true,
            delay_ms: Some(42),
            last_try: 0,
            burst: false,
        }];
        assert!(list.update_snapshot(&snapshot, NodeSort::Api));
        assert_eq!(list.rows[0][0], "proxy");
        assert!(list.update_snapshot(&snapshot, NodeSort::Delay));
        assert_eq!(list.rows[0][0], "second");
        snapshot.nodes.clear();
        assert!(list.update_snapshot(&snapshot, NodeSort::Delay));
        assert!(list.rows.is_empty());
        assert_eq!(list.note.as_deref(), Some("核心未返回出站"));
    }

    #[test]
    fn selection_and_traffic_refresh_invalidate_cached_rows() {
        let mut snapshot = snapshot();
        let mut list = NodeList::default();
        list.update_snapshot(&snapshot, NodeSort::Delay);
        assert_eq!(list.rows[0][5], "↑ — / ↓ —");
        snapshot.traffic.insert(
            "proxy".into(),
            api::NodeTraffic {
                uplink: Some(0),
                downlink: Some(2048),
            },
        );
        assert!(list.update_snapshot(&snapshot, NodeSort::Delay));
        assert_eq!(list.rows[0][5], "↑ 0 B / ↓ 2.0 KiB");
        snapshot.selected_nodes.insert("proxy".into());
        assert!(list.update_snapshot(&snapshot, NodeSort::Delay));
        assert!(list.selected.contains("proxy"));
        assert!(!list.update_snapshot(&snapshot, NodeSort::Delay));
        snapshot.selected_nodes.clear();
        snapshot.traffic.clear();
        assert!(list.update_snapshot(&snapshot, NodeSort::Delay));
        assert!(list.selected.is_empty());
        assert_eq!(list.rows[0][5], "↑ — / ↓ —");
        assert_eq!(super::format_bytes(Some(u64::MAX)), "16.0 EiB");
    }
}
