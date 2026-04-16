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
            let timeout_ms = if n.expire_ms < 0 { 5000 } else { n.expire_ms };
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

    /// Used by the idle-hide branch once added; keeping the method live
    /// so the notification daemon surface-close path has an obvious hook.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }
}
