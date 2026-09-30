//! 状态栏（托盘）图标：StatusNotifierItem。
//!
//! 自己用 GDBus 实现 SNI 接口，不引入 libappindicator 等 C 依赖：
//! 在会话总线上导出 `/StatusNotifierItem` 对象（实现 `Activate` + 只读属性），
//! 再向 `org.kde.StatusNotifierWatcher` 注册（SNI 规范允许用**对象路径**注册，
//! 这样不需要额外申请 bus name）。
//!
//! 没有托盘服务的桌面环境自动降级：注册失败只记日志，不影响相框本体。

use std::cell::RefCell;
use std::rc::Rc;

const OBJECT_PATH: &str = "/StatusNotifierItem";

pub struct Tray {
    /// 点击回调（左键：显示/隐藏设置窗口）
    on_toggle: Rc<dyn Fn()>,
    /// 导出的对象注册 id（必须在对象存活期间保留）
    _reg: RefCell<Option<gio::RegistrationId>>,
}

/// 图标名解析：宿主（SNI）拿到 `IconName` 后要在**当前图标主题**里找得到图形，
/// 否则状态栏里什么都不显示。
/// 优先用传入的名字；找不到就按候选列表挑第一个真实存在的，最后退回原名。
fn resolve_icon_name(preferred: &str) -> String {
    const CANDIDATES: [&str; 6] = [
        "emblem-photos-symbolic",   // breeze / breeze-dark
        "filter-photos-symbolic",   // Yaru
        "image-x-generic-symbolic", // Adwaita
        "multimedia-photos-symbolic",
        "folder-pictures-symbolic",
        "image-missing-symbolic",
    ];
    use std::path::{Path, PathBuf};
    let home = std::env::var("HOME").unwrap_or_default();
    // 图标搜索根目录
    let mut roots: Vec<PathBuf> = Vec::new();
    for base in [
        std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share")),
        "/usr/share".to_string(),
        "/usr/local/share".to_string(),
    ] {
        roots.push(PathBuf::from(&base).join("icons"));
    }
    // omarchy 当前主题自带的图标
    roots.push(PathBuf::from(&home).join(".local/share/omarchy/current/theme/icons"));

    // 当前图标主题 + 它的 Inherits 链（宿主只按这条链找图标）
    let mut chain: Vec<String> = Vec::new();
    if let Some(t) = run_gsettings_icon_theme() {
        let mut cur = t;
        for _ in 0..8 {
            if chain.contains(&cur) {
                break;
            }
            chain.push(cur.clone());
            cur = inherit_of(&roots, &cur).unwrap_or_default();
            if cur.is_empty() {
                break;
            }
        }
    }
    let exists_in = |name: &str| -> bool {
        let hit = |dir: &Path| {
            dir.join(format!("{name}.svg")).is_file()
                || dir.join(format!("{name}.png")).is_file()
        };
        // 只在**活动主题链**与 omarchy 主题目录里找：
        // 别的主题（breeze 等）里有不算数 —— 宿主渲染时同样找不到。
        chain.iter().any(|t| roots.iter().any(|r| hit(&r.join(t))))
            || roots.last().is_some_and(|r| hit(r))
    };

    if exists_in(preferred) {
        return preferred.to_string();
    }
    for c in CANDIDATES {
        if exists_in(c) {
            crate::info!("托盘图标 {preferred} 不在活动图标主题（{chain:?}），改用 {c}");
            return c.to_string();
        }
    }
    preferred.to_string()
}

fn run_gsettings_icon_theme() -> Option<String> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().trim_matches('\'').to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 读 <root>/<theme>/index.theme 的 Inherits=（主题继承链）
fn inherit_of(roots: &[std::path::PathBuf], theme: &str) -> Option<String> {
    for r in roots {
        let p = r.join(theme).join("index.theme");
        if let Ok(txt) = std::fs::read_to_string(&p) {
            for line in txt.lines() {
                let line = line.trim();
                if let Some(v) = line.strip_prefix("Inherits=") {
                    let v = v.split(',').next().unwrap_or("").trim().trim_matches('\'');
                    if !v.is_empty() {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

impl Tray {
    /// 注册托盘图标；宿主不支持时返回 None
    pub fn new(icon_name: &str, tooltip: &str, on_toggle: impl Fn() + 'static) -> Option<Rc<Self>> {
        // 图标名要**实际存在于当前图标主题**，否则宿主拿不到图形 → 状态栏里
        // 什么都没有（实测：本机主题 Yaru-blue 没有 emblem-photos-symbolic，
        // 它只在 breeze/AdwaitaLegacy 里 → 图标不显示）。
        // 所以按候选列表挑第一个能找到的。
        let icon_name = resolve_icon_name(icon_name);
        let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
        let tooltip = tooltip.to_string();

        let this = Rc::new(Tray {
            on_toggle: Rc::new(on_toggle),
            _reg: RefCell::new(None),
        });

        // gio 0.22 的 DBusInterfaceInfo 没有 builder，用 XML 描述最直接
        const XML: &str = r#"<node>
  <interface name="org.kde.StatusNotifierItem">
    <method name="Activate"><arg name="x" type="i"/><arg name="y" type="i"/></method>
    <method name="SecondaryActivate"><arg name="x" type="i"/><arg name="y" type="i"/></method>
    <property name="Category" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconPixmap" type="(iiay)" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
  </interface>
</node>"#;
        let node = gio::DBusNodeInfo::for_xml(XML).ok()?;
        let Some(info) = node.interfaces().first().cloned() else {
            crate::warn!("托盘接口描述解析失败");
            return None;
        };
        let info = &info;

        let icon_name = icon_name.to_string();
        let cb = this.on_toggle.clone();
        let reg = conn
            .register_object(OBJECT_PATH, &info)
            .method_call(move |_conn, _sender, _path, _iface, method, params, _inv| {
                crate::debug!("托盘收到调用：{method} params={params:?}");
                match method {
                    "Activate" | "SecondaryActivate" | "ContextMenu" => (cb)(),
                    _ => {}
                }
            })
            .property(move |_conn, _sender, _path, _iface, prop| {
                let v: glib::Variant = match prop {
                    "Category" => glib::Variant::from("ApplicationStatus"),
                    "Status" => glib::Variant::from("Active"),
                    "Id" => glib::Variant::from("omaframe"),
                    "Title" => glib::Variant::from("桌面相框"),
                    "IconName" => glib::Variant::from(icon_name.clone()),
                    // 签名必须是 (sa(iiay)ss)：图标名、图标像素数组 a(iiay)、标题、描述。
                    // 之前写成 (("omaframe",), Vec<(i32,i32,i32,i32)>, tooltip) →
                    // 少一个字段且类型不对，quickshell 每 30 秒报一次 DBus 签名错误（刷日志）。
                    "ToolTip" => {
                        let pixmaps: Vec<(i32, i32, Vec<u8>)> = Vec::new();
                        glib::Variant::from((
                            "omaframe",
                            pixmaps,
                            "桌面相框",
                            tooltip.clone(),
                        ))
                    }
                    "ItemIsMenu" => glib::Variant::from(false),
                    // SNI 规定：宿主优先用 IconName；解析不到时回退 IconPixmap。
                    // 自绘一张（不依赖图标主题）→ 主题里没有该图标名也能显示。
                    "IconPixmap" => glib::Variant::from(icon_pixmap_variant()),
                    _ => glib::Variant::from(""),
                };
                v
            })
            .build();
        match reg {
            Ok(id) => *this._reg.borrow_mut() = Some(id),
            Err(e) => {
                crate::warn!("托盘接口导出失败：{e}");
                return None;
            }
        }

        // 申请 bus 名字并向托盘宿主注册。
        //
        // **必须用每实例唯一的名字**（SNI 规范：org.kde.StatusNotifierItem-<pid>-<n>）。
        // 之前用的是固定名 `org.photoframe.StatusNotifierItem` + REPLACE_EXISTING，
        // 后果是：watcher（quickshell）会把每次重启当成同一个项，
        // 列表里只留下上一实例的**死条目**（实测 RegisteredStatusNotifierItems
        // 里根本没有 org.photoframe.StatusNotifierItem）→ 状态栏图标时有时无。
        // 唯一名 + 进程退出后名字自动消失，watcher 才能正确跟踪。
        let uniq = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let named = match conn.call_sync(
            // 必须显式写总线守护进程的名字：传 None 会用连接自己的唯一名，导致 Invalid method call
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            // 显式构造参数元组（(String, u32) 的 From 实现可能编码成 dict）
            // flags = 4 = DO_NOT_QUEUE：拿不到就直接失败，不排队等待
            Some(&glib::Variant::tuple_from_iter([
                glib::Variant::from(uniq.as_str()),
                glib::Variant::from(4u32),
            ])),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        ) {
            // RequestName 的回复是 "u"（uint32），在 Variant 里是**单元素元组**，
            // 要用 (u32,) 解包 —— 直接 get::<u32>() 会失败并误判成“名字没拿到”。
            // 返回值：1=PRIMARY_OWNER 4=ALREADY_OWNER 才算拿到。
            Ok(r) => {
                let code: u32 = r
                    .get::<(u32,)>()
                    .map(|(c,)| c)
                    .unwrap_or_else(|| r.child_value(0).get::<u32>().unwrap_or(0));
                if code == 1 || code == 4 {
                    true
                } else {
                    crate::warn!("申请 bus 名字未成为主拥有者（code={code}）");
                    false
                }
            }
            Err(e) => {
                crate::warn!("申请 bus 名字失败：{e}");
                false
            }
        };
        crate::debug!("已持有 bus 名字 {uniq}: {named}");
        // 拿到唯一名就用名字注册（quickshell 等宿主只认能对应上的名字）
        let service = if named { uniq.as_str() } else { OBJECT_PATH };

        // 向托盘宿主注册。宿主（quickshell）对"刚启动就注册"的项可能来不及握手，
        // 因此延迟一小会儿并重试。
        let conn2 = conn.clone();
        let service = service.to_string();
        let register = move |conn: &gio::DBusConnection, service: &str| -> Result<(), glib::Error> {
            conn.call_sync(
                Some("org.kde.StatusNotifierWatcher"),
                "/StatusNotifierWatcher",
                "org.kde.StatusNotifierWatcher",
                "RegisterStatusNotifierItem",
                // parameters 必须是"参数元组"，传裸字符串会变成畸形消息
                Some(&glib::Variant::tuple_from_iter([glib::Variant::from(service)])),
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
            )
            .map(|_| ())
        };
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            match register(&conn2, &service) {
                Ok(()) => {
                    crate::info!("托盘图标已注册（左键切换设置窗口）");
                }
                Err(e) => {
                    crate::warn!("托盘注册失败，700ms 后重试：{e}");
                    let conn3 = conn2.clone();
                    let svc = service.clone();
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(700),
                        move || match register(&conn3, &svc) {
                            Ok(()) => crate::info!("托盘图标已注册（左键切换设置窗口）"),
                            Err(e) => crate::warn!("托盘注册仍失败：{e}"),
                        },
                    );
                }
            }
        });
        Some(this)
    }
}

/// 自绘托盘图标 → SNI 的 IconPixmap（ARGB32，自上而下，像素数组）。
/// 不依赖图标主题：宿主解析不到 IconName 时用它兜底。
/// 画一个"相框"图形：深色圆角外框 + 浅色内区 + 小圆（太阳）。
fn icon_pixmap_variant() -> glib::Variant {
    const S: i32 = 22;
    let mut data: Vec<u8> = vec![0; (S * S * 4) as usize];
    {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, S, S)
            .expect("托盘图标 surface");
        {
            let cr = cairo::Context::new(&surface).expect("托盘图标 cairo");
            // 深板岩色：与顶栏其它系统图标同一色系（深板岩/蓝灰），
            // 在浅色顶栏上不再"发白"。
            cr.set_source_rgba(0.16, 0.18, 0.22, 1.0);
            // 圆角矩形边框
            let r = 3.0;
            cr.move_to(r, 0.5);
            cr.line_to(S as f64 - r, 0.5);
            cr.arc(S as f64 - r, 0.5 + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
            cr.line_to(S as f64 - 0.5, S as f64 - r);
            cr.arc(S as f64 - 0.5, S as f64 - r, r, 0.0, std::f64::consts::FRAC_PI_2);
            cr.line_to(r, S as f64 - 0.5);
            cr.arc(0.5, S as f64 - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
            cr.line_to(0.5, r);
            cr.arc(0.5, r, r, std::f64::consts::PI, std::f64::consts::FRAC_PI_2 * 3.0);
            cr.close_path();
            cr.fill().ok();

            // 内区（"照片"）：中深蓝灰，保持整体偏暗
            cr.set_source_rgba(0.42, 0.47, 0.55, 1.0);
            cr.rectangle(3.5, 3.5, S as f64 - 7.0, S as f64 - 7.0);
            cr.fill().ok();

            // 一个小圆（太阳）
            cr.set_source_rgba(0.70, 0.74, 0.80, 1.0);
            cr.arc(S as f64 - 7.0, 7.0, 2.0, 0.0, std::f64::consts::TAU);
            cr.fill().ok();
        }
        surface.mark_dirty();
        // 读回像素（cairo ARGB32 已是预乘 + 本机字节序 BGRA）
        let _ = surface.with_data(|d| {
            let stride = surface.stride() as usize;
            for y in 0..S as usize {
                for x in 0..S as usize {
                    let si = y * stride + x * 4;
                    let di = (y * S as usize + x) * 4;
                    data[di] = d[si + 2]; // B
                    data[di + 1] = d[si + 1]; // G
                    data[di + 2] = d[si]; // R
                    data[di + 3] = d[si + 3]; // A
                }
            }
        });
    }
    // (iiay) = (宽, 高, 字节数组)
    glib::Variant::from((S, S, data))
}
