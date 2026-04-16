use std::collections::HashMap;
use std::sync::mpsc;

pub enum DbusEvent {
    Notify {
        app_name: String,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<(String, String)>, // (id, label) pairs
        urgency: u8, // 0=low, 1=normal, 2=critical
        expire_timeout: i32,
        reply: mpsc::SyncSender<u32>,
    },
    Close {
        id: u32,
    },
}

struct NotificationsService {
    tx: mpsc::Sender<DbusEvent>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationsService {
    fn get_capabilities(&self) -> Vec<String> {
        vec![
            "body".to_string(),
            "body-markup".to_string(),
            "actions".to_string(),
        ]
    }

    fn notify(
        &self,
        app_name: String,
        _replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let urgency = hints
            .get("urgency")
            .and_then(|v| <u8>::try_from(v).ok())
            .unwrap_or(1);

        // Actions come as [id, label, id, label, ...] — pair them.
        let action_pairs: Vec<(String, String)> = actions
            .chunks_exact(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect();

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(DbusEvent::Notify {
                app_name,
                app_icon,
                summary,
                body,
                actions: action_pairs,
                urgency,
                expire_timeout,
                reply: reply_tx,
            })
            .ok();

        reply_rx.recv().unwrap_or(0)
    }

    fn close_notification(&self, id: u32) {
        self.tx.send(DbusEvent::Close { id }).ok();
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "kara-whisper".into(),
            "kara".into(),
            "0.1.0".into(),
            "1.2".into(),
        )
    }
}

/// XDG Desktop Portal Settings — advertises dark mode preference to applications.
/// Implements org.freedesktop.portal.Settings so Floorp/Firefox and GTK apps
/// pick up the color scheme.
struct PortalSettings {
    color_scheme: u32, // 0 = no preference, 1 = dark, 2 = light
}

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl PortalSettings {
    fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>> {
        let mut result: HashMap<String, HashMap<String, zbus::zvariant::OwnedValue>> = HashMap::new();
        for ns in &namespaces {
            if ns == "org.freedesktop.appearance" || ns.is_empty() || ns == "*" {
                let mut appearance = HashMap::new();
                appearance.insert(
                    "color-scheme".to_string(),
                    zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(self.color_scheme)).unwrap(),
                );
                result.insert("org.freedesktop.appearance".to_string(), appearance);
            }
        }
        result
    }

    fn read(
        &self,
        namespace: &str,
        key: &str,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedValue> {
        if namespace == "org.freedesktop.appearance" && key == "color-scheme" {
            Ok(zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(self.color_scheme)).unwrap())
        } else {
            Err(zbus::fdo::Error::UnknownProperty(format!(
                "{namespace}.{key}"
            )))
        }
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        2
    }
}

pub fn spawn_dbus(tx: mpsc::Sender<DbusEvent>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let conn = match zbus::blocking::Connection::session() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("kara-whisper: failed to connect to D-Bus: {e}");
                return;
            }
        };

        // Register notifications service
        if let Err(e) = conn
            .object_server()
            .at("/org/freedesktop/Notifications", NotificationsService { tx })
        {
            eprintln!("kara-whisper: failed to register D-Bus object: {e}");
            return;
        }

        if let Err(e) = conn.request_name("org.freedesktop.Notifications") {
            eprintln!("kara-whisper: failed to acquire D-Bus name (is another notification daemon running?): {e}");
            return;
        }

        // Register portal settings (dark mode) on a separate connection
        // since it needs a different well-known name
        if let Ok(portal_conn) = zbus::blocking::Connection::session() {
            let settings = PortalSettings { color_scheme: 1 }; // 1 = prefer dark
            if portal_conn
                .object_server()
                .at("/org/freedesktop/portal/desktop", settings)
                .is_ok()
            {
                if portal_conn
                    .request_name("org.freedesktop.portal.Desktop")
                    .is_ok()
                {
                    eprintln!("kara-whisper: portal settings registered (dark mode)");
                } else {
                    eprintln!("kara-whisper: portal name taken (xdg-desktop-portal running?)");
                }
            }
        }

        eprintln!("kara-whisper: D-Bus services registered");

        // Keep thread alive — zbus internal executor handles message dispatch
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    })
}
