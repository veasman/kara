use std::collections::HashMap;
use std::sync::mpsc;

pub enum DbusEvent {
    Notify {
        app_name: String,
        summary: String,
        body: String,
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
        vec!["body".to_string()]
    }

    fn notify(
        &self,
        app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let urgency = hints
            .get("urgency")
            .and_then(|v| <u8>::try_from(v).ok())
            .unwrap_or(1);

        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(DbusEvent::Notify {
                app_name,
                summary,
                body,
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

pub fn spawn_dbus(tx: mpsc::Sender<DbusEvent>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let conn = match zbus::blocking::Connection::session() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("kara-whisper: failed to connect to D-Bus: {e}");
                return;
            }
        };

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

        eprintln!("kara-whisper: D-Bus service registered");

        // Keep thread alive — zbus internal executor handles message dispatch
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    })
}
