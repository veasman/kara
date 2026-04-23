use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>, // (id, label) pairs
    pub urgency: Urgency,
    pub expire_ms: i32, // -1 = server decides, 0 = never
    pub created_at: Instant,
}

impl Notification {
    /// Actions that should render as visible buttons on the card.
    ///
    /// The freedesktop Notifications spec carves out a special action
    /// key "default" as the implicit click-target — clicking anywhere
    /// on the notification body should invoke it. Per the spec,
    /// "default" must NOT appear in the button row. Apps that don't
    /// follow that convention (Thunderbird ships `("default",
    /// "Activate")` on every mail alert) ended up rendering a button
    /// labeled "Activate" that duplicated body-click behavior — the
    /// "weird wording" the user flagged. Hide it. Body click still
    /// fires the action (see `Whisper::handle_click`).
    pub fn button_actions(&self) -> impl Iterator<Item = &(String, String)> {
        self.actions.iter().filter(|(id, _)| id != "default")
    }

    pub fn has_button_actions(&self) -> bool {
        self.button_actions().next().is_some()
    }

    /// The implicit default-action id for this notification, if the
    /// app sent one. Used by body-click so `Activate on mail`, `Open`
    /// on browser notifications, etc. still reach the sender.
    pub fn default_action_id(&self) -> Option<&str> {
        self.actions
            .iter()
            .find(|(id, _)| id == "default")
            .map(|(id, _)| id.as_str())
    }
}

pub struct NotificationQueue {
    notifications: Vec<Notification>,
    next_id: u32,
}

impl NotificationQueue {
    pub fn new() -> Self {
        Self {
            notifications: Vec::new(),
            next_id: 1,
        }
    }

    pub fn add(
        &mut self,
        app_name: String,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<(String, String)>,
        urgency: Urgency,
        expire_ms: i32,
    ) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.notifications.push(Notification {
            id,
            app_name,
            app_icon,
            summary,
            body,
            actions,
            urgency,
            expire_ms,
            created_at: Instant::now(),
        });
        id
    }

    pub fn remove(&mut self, id: u32) {
        self.notifications.retain(|n| n.id != id);
    }

    /// Remove expired notifications, return their IDs.
    pub fn tick(&mut self) -> Vec<u32> {
        let now = Instant::now();
        let mut expired = Vec::new();
        self.notifications.retain(|n| {
            if n.urgency == Urgency::Critical {
                return true; // never auto-expire
            }
            // Default timeout (server-decides sentinel `-1`) is 12s —
            // long enough to actually read a multi-line notification
            // without chasing it with the pointer. Client-specified
            // timeouts still win, and `0` still means "never
            // auto-expire". Earlier this was 8s; in practice emails
            // and build results flashed past before the user could
            // finish reading the subject line.
            let timeout_ms = if n.expire_ms < 0 { 12_000 } else { n.expire_ms };
            if timeout_ms == 0 {
                return true; // 0 = never expire
            }
            let elapsed = now.duration_since(n.created_at).as_millis() as i32;
            if elapsed >= timeout_ms {
                expired.push(n.id);
                false
            } else {
                true
            }
        });
        expired
    }

    pub fn visible(&self) -> &[Notification] {
        &self.notifications
    }

    /// Look up a notification by id. Used on body-click to read the
    /// sender's `default` action so the daemon can emit ActionInvoked
    /// before closing the card.
    pub fn find(&self, id: u32) -> Option<&Notification> {
        self.notifications.iter().find(|n| n.id == id)
    }

    /// Used by the idle-hide branch once added; keeping the method live
    /// so the notification daemon surface-close path has an obvious hook.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }
}
