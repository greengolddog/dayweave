import SwiftUI

struct QuickCaptureView: View {
    @Environment(\.dismiss) private var dismiss
    @EnvironmentObject private var store: PlannerStore
    @FocusState private var titleIsFocused: Bool

    private let itemID: UUID
    @State private var state: CanonicalItemEditorState
    @State private var showsDetails = false
    @State private var saveError: String?

    init(
        itemID: UUID = UUID(),
        now: Date = Date(),
        profileTimezoneName: String
    ) {
        self.itemID = itemID
        _state = State(initialValue: CanonicalItemEditorState(
            itemID: itemID,
            now: now,
            timezoneName: profileTimezoneName
        ))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 14) {
                Image(systemName: "tray.and.arrow.down.fill")
                    .font(.title2)
                    .foregroundStyle(.tint)
                    .frame(width: 42, height: 42)
                    .background(.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 11))
                VStack(alignment: .leading, spacing: 3) {
                    Text("Quick Capture").font(.title2.weight(.semibold))
                    Text("A title is enough. This creates an Inbox item, not a schedule block.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("quick-capture.cancel")
            }

            TextField("What do you want to remember?", text: $state.title)
                .textFieldStyle(.roundedBorder)
                .font(.title3)
                .focused($titleIsFocused)
                .privacySensitive(state.isSensitive)
                .accessibilityIdentifier("quick-capture.title")

            HStack {
                Text("\(state.title.unicodeScalars.count)/\(DayWeaveCanonicalItemDraft.maximumTitleScalars) characters")
                Spacer()
                Label("Inbox", systemImage: "tray")
            }
            .font(.caption)
            .foregroundStyle(.secondary)

            DisclosureGroup("Optional details", isExpanded: $showsDetails) {
                VStack(alignment: .leading, spacing: 14) {
                    Picker("Type", selection: $state.kind) {
                        Text("Task").tag(DayWeaveCanonicalItemKind.task)
                        Text("Habit").tag(DayWeaveCanonicalItemKind.habit)
                        Text("Routine").tag(DayWeaveCanonicalItemKind.routine)
                        Text("Goal").tag(DayWeaveCanonicalItemKind.goal)
                        Text("Event").tag(DayWeaveCanonicalItemKind.event)
                        Text("Break").tag(DayWeaveCanonicalItemKind.breakTime)
                    }
                    .accessibilityIdentifier("quick-capture.kind")

                    TextField("Notes", text: $state.notes, axis: .vertical)
                        .textFieldStyle(.roundedBorder)
                        .lineLimit(2...5)
                        .privacySensitive(state.isSensitive)
                        .accessibilityIdentifier("quick-capture.notes")

                    Toggle(isOn: $state.isSensitive) {
                        VStack(alignment: .leading, spacing: 2) {
                            Label("Sensitive", systemImage: "checkmark.shield")
                            Text("Assistant context receives only anonymous occupied time.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityIdentifier("quick-capture.sensitive")

                    if state.kind == .event {
                        DatePicker(
                            "Starts",
                            selection: $state.eventStart,
                            displayedComponents: [.date, .hourAndMinute]
                        )
                        .environment(\.timeZone, editorTimeZone)
                        DatePicker(
                            "Ends",
                            selection: $state.eventEnd,
                            displayedComponents: [.date, .hourAndMinute]
                        )
                        .environment(\.timeZone, editorTimeZone)
                    } else {
                        Toggle("Add a duration estimate", isOn: $state.hasDuration)
                        if state.hasDuration {
                            Stepper(value: durationMinutes, in: 1...527_040, step: 5) {
                                LabeledContent(
                                    "Duration",
                                    value: CanonicalItemEditorState.durationDescription(
                                        state.durationSeconds
                                    )
                                )
                            }
                        }
                    }

                    if state.kind == .habit {
                        Label("Quick Capture starts habits as daily; use Edit for another cadence.", systemImage: "repeat")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                .padding(.top, 12)
            }
            .accessibilityIdentifier("quick-capture.details")

            if let message = saveError ?? state.validationIssue {
                Label(message, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: 9))
                    .accessibilityIdentifier("quick-capture.diagnostic")
            }

            HStack {
                Label("Encrypted locally until sync", systemImage: "lock")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("Add to Inbox", action: save)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canSave)
                    .accessibilityIdentifier("quick-capture.save")
            }
        }
        .padding(24)
        .frame(width: 560)
        .onAppear { titleIsFocused = true }
        .onChange(of: state.kind) { _, _ in
            state.readyStatus = .inbox
            state.normalizeForKindChange()
        }
        .accessibilityIdentifier("quick-capture")
    }

    private var canSave: Bool {
        store.canMutatePlan && state.validationIssue == nil
    }

    private var durationMinutes: Binding<Int> {
        Binding(
            get: { max(1, Int(state.durationSeconds) / 60) },
            set: { minutes in
                let seconds = min(
                    UInt64(DayWeaveCanonicalItemDraft.maximumDurationSeconds),
                    UInt64(max(1, minutes)) * 60
                )
                state.durationSeconds = UInt32(seconds)
            }
        )
    }

    private var editorTimeZone: TimeZone {
        PlannerTimeZone.resolve(state.timezoneName)
    }

    private func save() {
        guard canSave else { return }
        do {
            state.readyStatus = .inbox
            try store.enqueueCanonicalCreate(itemID: itemID, draft: state.draft)
            store.selectCanonicalItem(itemID)
            dismiss()
        } catch {
            saveError = error.localizedDescription
        }
    }
}
