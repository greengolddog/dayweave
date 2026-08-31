import AppKit

/// Announces transient state changes that do not naturally receive keyboard
/// focus. Keep announcement text free of planner content unless the user is
/// already editing that same content.
@MainActor
func dayWeavePostAccessibilityAnnouncement(
    _ message: String,
    priority: NSAccessibilityPriorityLevel = .medium
) {
    let message = message.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !message.isEmpty else { return }
    NSAccessibility.post(
        element: NSApplication.shared,
        notification: .announcementRequested,
        userInfo: [
            .announcement: message,
            .priority: priority.rawValue,
        ]
    )
}
