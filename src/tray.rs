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

impl Tray {
    /// 注册托盘图标；宿主不支持时返回 None
    pub fn new(icon_name: &str, tooltip: &str, on_toggle: impl Fn() + 'static) -> Option<Rc<Self>> {
        let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;

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
        let tooltip = tooltip.to_string();
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
                    "Id" => glib::Variant::from("photo-frame"),
                    "Title" => glib::Variant::from("桌面相框"),
                    "IconName" => glib::Variant::from(icon_name.clone()),
                    // 签名必须是 (sa(iiay)ss)：图标名、图标像素数组 a(iiay)、标题、描述。
                    // 之前写成 (("photo-frame",), Vec<(i32,i32,i32,i32)>, tooltip) →
                    // 少一个字段且类型不对，quickshell 每 30 秒报一次 DBus 签名错误（刷日志）。
                    "ToolTip" => {
                        let pixmaps: Vec<(i32, i32, Vec<u8>)> = Vec::new();
                        glib::Variant::from((
                            "photo-frame",
                            pixmaps,
                            "桌面相框",
                            tooltip.clone(),
                        ))
                    }
                    "ItemIsMenu" => glib::Variant::from(false),
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

        // 申请一个 well-known bus name：部分 SNI 宿主（含 quickshell）
        // 只认"用名字注册"，所以先 RequestName 再用该名字注册
        const WELL_KNOWN: &str = "org.photoframe.StatusNotifierItem";
        let named = match conn.call_sync(
            // 必须显式写总线守护进程的名字：传 None 会用连接自己的唯一名，导致 Invalid method call
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            // 显式构造参数元组（(String, u32) 的 From 实现可能编码成 dict）
            // flags = 4|8 = ALLOW_REPLACEMENT | REPLACE_EXISTING：
            // 允许顶掉上一个还没退干净的实例
            Some(&glib::Variant::tuple_from_iter([
                glib::Variant::from(WELL_KNOWN),
                glib::Variant::from(4u32 | 8u32),
            ])),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        ) {
            // 调用成功即视为拿到名字（已用 REPLACE_EXISTING 允许顶掉旧实例）
            Ok(_) => true,
            Err(e) => {
                crate::warn!("申请 bus 名字失败：{e}");
                false
            }
        };
        crate::debug!("已持有 bus 名字 {WELL_KNOWN}: {named}");
        // 拿到 well-known 名字就用名字注册（quickshell 等宿主只认名字）
        let service = if named { WELL_KNOWN } else { OBJECT_PATH };

        // 向托盘宿主注册。宿主（quickshell）对"刚启动就注册"的项可能来不及握手，
        // 因此延迟一小会儿并重试一次。
        let service = service.to_string();
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            for attempt in 0..2 {
                match conn.call_sync(
                    Some("org.kde.StatusNotifierWatcher"),
                    "/StatusNotifierWatcher",
                    "org.kde.StatusNotifierWatcher",
                    "RegisterStatusNotifierItem",
                    // 注意：parameters 必须是"参数元组"，传裸字符串会变成畸形消息
                    Some(&glib::Variant::tuple_from_iter([glib::Variant::from(
                        service.as_str(),
                    )])),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    gio::Cancellable::NONE,
                ) {
                    Ok(_) => {
                        crate::info!("托盘图标已注册（左键切换设置窗口）");
                        return;
                    }
                    Err(e) => {
                        crate::warn!("托盘注册失败（第 {attempt} 次）：{e}");
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(700),
                            move || {
                                let _ = conn.call_sync(
                                    Some("org.kde.StatusNotifierWatcher"),
                                    "/StatusNotifierWatcher",
                                    "org.kde.StatusNotifierWatcher",
                                    "RegisterStatusNotifierItem",
                                    Some(&glib::Variant::from(service.as_str())),
                                    None,
                                    gio::DBusCallFlags::NONE,
                                    -1,
                                    gio::Cancellable::NONE,
                                );
                            },
                        );
                        return;
                    }
                }
            }
        });
        Some(this)
    }
}
