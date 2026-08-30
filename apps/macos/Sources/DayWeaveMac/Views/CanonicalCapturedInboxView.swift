import SwiftUI

struct CanonicalCapturedInboxView: View {
    @EnvironmentObject private var store: PlannerStore
    @State private var editorRoute: CanonicalInboxEditorRoute?
    @State private var isQuickCapturePresented = false
    @State private var actionError: String?

    private var presentation: CanonicalInboxPresentation {
        CanonicalInboxPresentation.build(
            activeItems: store.canonicalItems,
            pendingMutations: store.pendingCanonicalAuthoringMutations,
            trashEntries: store.canonicalTrash,
            sensitivityPresentation: {
                store.canonicalSensitivityPresentation(itemID: $0)
            }
        )
    }

    var body: some View {
        List {
            Section {
                HStack(spacing: 14) {
                    Image(systemName: "tray.full.fill")
                        .font(.title2)
                        .foregroundStyle(.tint)
                        .frame(width: 42, height: 42)
                        .background(.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 11))
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Captured items").font(.headline)
                        Text("Inbox items wait for a decision. Planned items are eligible for composition after sync.")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Quick Capture") { isQuickCapturePresented = true }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut("n", modifiers: .command)
                        .disabled(!store.canMutatePlan)
                        .accessibilityIdentifier("canonical-inbox.quick-capture")
                }

                if let actionError {
                    Label(actionError, systemImage: "exclamationmark.triangle")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .accessibilityIdentifier("canonical-inbox.diagnostic")
                }

                if !store.canMutatePlan {
                    Label("Planner changes are temporarily locked; captured items remain readable.", systemImage: "lock")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            rowsSection(
                title: "Inbox",
                symbol: "tray",
                rows: presentation.inbox,
                emptyMessage: "Nothing is waiting for triage."
            )
            rowsSection(
                title: "Planned",
                symbol: "checkmark.circle",
                rows: presentation.planned,
                emptyMessage: "Move an item to Planned when it is ready for composition."
            )
            rowsSection(
                title: "Conflicts",
                symbol: "exclamationmark.triangle",
                rows: presentation.conflicts,
                emptyMessage: "No authoring conflicts need review."
            )
            rowsSection(
                title: "Recently Deleted",
                symbol: "trash",
                rows: presentation.trash,
                emptyMessage: "Deleted items available for restore appear here."
            )

            Section {
                Label(
                    "Create, edit, delete, and restore actions only queue encrypted local intent. This view never calls the network.",
                    systemImage: "lock.shield"
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Captured Inbox")
        .toolbar {
            ToolbarItemGroup {
                Button {
                    editorRoute = .init(mode: .create(itemID: UUID()))
                } label: {
                    Label("New Detailed Item", systemImage: "square.and.pencil")
                }
                .disabled(!store.canMutatePlan)
                .help("Create an item with full planning details")
                .accessibilityIdentifier("canonical-inbox.new-detailed")

                Button {
                    isQuickCapturePresented = true
                } label: {
                    Label("Quick Capture", systemImage: "plus")
                }
                .disabled(!store.canMutatePlan)
                .help("Capture a title directly into Inbox")
            }
        }
        .sheet(item: $editorRoute) { route in
            CanonicalItemEditorView(
                mode: route.mode,
                readOnlyDiagnostic: route.readOnlyDiagnostic
            )
            .environmentObject(store)
        }
        .sheet(isPresented: $isQuickCapturePresented) {
            QuickCaptureView()
                .environmentObject(store)
        }
        .accessibilityIdentifier("canonical-inbox")
    }

    @ViewBuilder
    private func rowsSection(
        title: String,
        symbol: String,
        rows: [CanonicalInboxPresentation.Row],
        emptyMessage: String
    ) -> some View {
        Section {
            if rows.isEmpty {
                Label(emptyMessage, systemImage: symbol)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .padding(.vertical, 6)
            } else {
                ForEach(rows) { row in
                    CanonicalCapturedInboxRow(
                        row: row,
                        identifierScope: title.lowercased()
                            .replacingOccurrences(of: " ", with: "-"),
                        isSelected: store.selectedCanonicalItemID == row.itemID,
                        canMutate: store.canMutatePlan,
                        select: { store.selectCanonicalItem(row.itemID) },
                        edit: { presentEditor(for: row) },
                        trash: { enqueueTrash(row.itemID) },
                        restore: { enqueueRestore(row.itemID) },
                        discard: row.mutationID.map { mutationID in
                            { discard(mutationID) }
                        },
                        duplicateConflictedDraft: duplicateAction(for: row)
                    )
                }
            }
        } header: {
            Label("\(title) · \(rows.count)", systemImage: symbol)
        }
    }

    private func presentEditor(for row: CanonicalInboxPresentation.Row) {
        actionError = nil
        if let mutationID = row.mutationID,
           let mutation = store.canonicalAuthoringMutation(id: mutationID),
           let draft = mutation.draft,
           mutation.operation == .create || mutation.operation == .replace {
            editorRoute = .init(
                mode: .updatePending(
                    mutationID: mutation.id,
                    itemID: mutation.itemID,
                    draft: draft
                ),
                readOnlyDiagnostic: readOnlyDiagnostic(for: row, mutation: mutation)
            )
            return
        }
        let mutation = row.mutationID.flatMap {
            store.canonicalAuthoringMutation(id: $0)
        }
        guard let item = store.canonicalItems.first(where: { $0.id == row.itemID }) else {
            actionError = "The selected item is no longer available."
            return
        }
        editorRoute = .init(
            mode: .replace(itemID: item.id, draft: DayWeaveCanonicalItemDraft(item: item)),
            readOnlyDiagnostic: readOnlyDiagnostic(
                for: row,
                mutation: mutation,
                item: item
            )
        )
    }

    private func readOnlyDiagnostic(
        for row: CanonicalInboxPresentation.Row,
        mutation: DayWeavePendingCanonicalAuthoringMutation? = nil,
        item: DayWeaveCanonicalItem? = nil
    ) -> String? {
        if row.hasHierarchyCycle {
            return "This item is part of a hierarchy cycle. Resolve the server hierarchy before editing it."
        }
        if row.hasMissingParent {
            return "This item's parent is unavailable. Restore or synchronize the parent before editing it."
        }
        if mutation?.disposition == .conflicted {
            if mutation?.operation == .restore {
                return "This item was restored elsewhere with different content. The active version is shown read-only; choose Keep Active Version to discard the retained restore conflict."
            }
            return "This exact queued change conflicted with canonical state. Copy it as a new editable Inbox item to keep working without discarding this recovery record."
        }
        if mutation?.hasBeenSubmitted == true || mutation?.configurationIdentifier != nil {
            return "This exact queued request is bound for synchronization and cannot be edited until recovery finishes."
        }
        if let item, !item.supportsCanonicalAuthoringReplacement {
            return "This item contains fields that the typed editor cannot replace safely."
        }
        return nil
    }

    private func enqueueTrash(_ itemID: UUID) {
        performAction { try store.enqueueCanonicalTrash(itemID: itemID) }
    }

    private func enqueueRestore(_ itemID: UUID) {
        performAction { try store.enqueueCanonicalRestore(itemID: itemID) }
    }

    private func discard(_ mutationID: UUID) {
        performAction { try store.discardCanonicalAuthoringMutation(mutationID) }
    }

    private func duplicateAction(
        for row: CanonicalInboxPresentation.Row
    ) -> (() -> Void)? {
        guard case .conflicted = row.syncState,
              row.source == .localCreate || row.source == .pendingReplace,
              let mutationID = row.mutationID else { return nil }
        return { duplicateConflictedDraft(mutationID) }
    }

    private func duplicateConflictedDraft(_ mutationID: UUID) {
        do {
            let copy = try store.duplicateConflictedCanonicalDraft(mutationID)
            guard copy.operation == .create, let draft = copy.draft else {
                actionError = "The copied Inbox item could not be opened safely."
                return
            }
            actionError = nil
            editorRoute = .init(mode: .updatePending(
                mutationID: copy.id,
                itemID: copy.itemID,
                draft: draft
            ))
        } catch {
            actionError = error.localizedDescription
        }
    }

    private func performAction(_ action: () throws -> Void) {
        do {
            try action()
            actionError = nil
        } catch {
            actionError = error.localizedDescription
        }
    }
}

private struct CanonicalInboxEditorRoute: Identifiable {
    let id = UUID()
    let mode: CanonicalItemEditorMode
    let readOnlyDiagnostic: String?

    init(mode: CanonicalItemEditorMode, readOnlyDiagnostic: String? = nil) {
        self.mode = mode
        self.readOnlyDiagnostic = readOnlyDiagnostic
    }
}

private struct CanonicalCapturedInboxRow: View {
    let row: CanonicalInboxPresentation.Row
    let identifierScope: String
    let isSelected: Bool
    let canMutate: Bool
    let select: () -> Void
    let edit: () -> Void
    let trash: () -> Void
    let restore: () -> Void
    let discard: (() -> Void)?
    let duplicateConflictedDraft: (() -> Void)?

    private let maximumVisibleDepth = 5

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Color.clear
                .frame(width: CGFloat(min(row.depth, maximumVisibleDepth)) * 14)

            Image(systemName: kindSymbol)
                .foregroundStyle(kindColor)
                .frame(width: 32, height: 32)
                .background(kindColor.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))

            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(row.title)
                        .font(.headline)
                        .lineLimit(2)
                        .privacySensitive(row.isSensitive)
                    if row.depth > maximumVisibleDepth {
                        Text("Level \(row.depth + 1)")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(.quaternary, in: Capsule())
                    }
                    Spacer()
                    syncChip
                }

                if !row.breadcrumb.isEmpty {
                    Text(row.breadcrumb.joined(separator: " › "))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .privacySensitive(row.isSensitive)
                }

                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 6) { metadataChips }
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(spacing: 6) {
                            statusChip
                            privacyChip
                        }
                        HStack(spacing: 6) {
                            durationChip
                            deadlineChip
                        }
                    }
                }

                if row.hasHierarchyCycle || row.hasMissingParent {
                    Label(
                        row.hasHierarchyCycle ? "Hierarchy cycle" : "Parent unavailable",
                        systemImage: "exclamationmark.triangle"
                    )
                    .font(.caption)
                    .foregroundStyle(.orange)
                }
            }

            actionControls
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 8)
        .background(
            isSelected ? Color.accentColor.opacity(0.11) : Color.clear,
            in: RoundedRectangle(cornerRadius: 10)
        )
        .overlay {
            if isSelected {
                RoundedRectangle(cornerRadius: 10)
                    .stroke(Color.accentColor.opacity(0.35), lineWidth: 1)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture(perform: select)
        .contextMenu { contextActions }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(row.accessibilitySummary)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
        .accessibilityIdentifier(
            "canonical-inbox.\(identifierScope).row.\(row.itemID.uuidString.lowercased())"
        )
    }

    @ViewBuilder
    private var actionControls: some View {
        if let duplicateConflictedDraft {
            Button("Copy as New", action: duplicateConflictedDraft)
                .controlSize(.small)
                .disabled(!canMutate)
                .help("Copy as new editable Inbox item")
                .accessibilityLabel("Copy as new editable Inbox item")
                .accessibilityIdentifier(
                    "canonical-inbox.\(identifierScope).copy-conflict.\(row.itemID.uuidString.lowercased())"
                )
        }

        switch row.source {
        case .canonical, .localCreate, .pendingReplace:
            Button(action: edit) {
                Image(systemName: row.isReadOnly ? "doc.text.magnifyingglass" : "pencil")
            }
            .buttonStyle(.borderless)
            .disabled(!canMutate && !row.isReadOnly)
            .help(row.isReadOnly ? "View read-only details" : "Edit item")
            .accessibilityLabel(row.isReadOnly ? "View item" : "Edit item")
            .accessibilityIdentifier(
                "canonical-inbox.\(identifierScope).edit.\(row.itemID.uuidString.lowercased())"
            )
        case .recentTrash:
            Button("Restore", action: restore)
                .controlSize(.small)
                .disabled(!canMutate)
                .accessibilityIdentifier(
                    "canonical-inbox.\(identifierScope).restore.\(row.itemID.uuidString.lowercased())"
                )
        case .pendingTrash, .pendingRestore:
            if let discard {
                Button("Cancel", action: discard)
                    .controlSize(.small)
                    .disabled(!canMutate || !mutationCanBeDiscarded)
            }
        case .activeRestore:
            Button(action: edit) {
                Image(systemName: "doc.text.magnifyingglass")
            }
            .buttonStyle(.borderless)
            .help("Review the active version and restore conflict")
            .accessibilityLabel("Review restore conflict")
            if let discard {
                Button("Keep Active", action: discard)
                    .controlSize(.small)
                    .disabled(!canMutate || !mutationCanBeDiscarded)
                    .help("Keep the active version and discard local restore intent")
            }
        }

        Menu {
            contextActions
        } label: {
            Image(systemName: "ellipsis.circle")
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .accessibilityLabel("More item actions")
    }

    @ViewBuilder
    private var contextActions: some View {
        switch row.source {
        case .canonical:
            Button("Edit…", action: edit)
                .disabled(!canMutate || row.isReadOnly)
            Button("Move to Recently Deleted", role: .destructive, action: trash)
                .disabled(!canMutate || row.isReadOnly)
        case .localCreate, .pendingReplace:
            if let duplicateConflictedDraft {
                Button(
                    "Copy as new editable Inbox item",
                    systemImage: "doc.on.doc",
                    action: duplicateConflictedDraft
                )
                .disabled(!canMutate)
            }
            Button("Edit queued change…", action: edit)
                .disabled(!canMutate || row.isReadOnly)
            if let discard {
                Button(
                    row.source == .localCreate ? "Discard local item" : "Keep server version",
                    role: .destructive,
                    action: discard
                )
                .disabled(!canMutate || !mutationCanBeDiscarded)
            }
        case .pendingTrash:
            if let discard {
                Button("Cancel deletion", action: discard)
                    .disabled(!canMutate || !mutationCanBeDiscarded)
            }
        case .pendingRestore:
            if let discard {
                Button("Cancel restore", action: discard)
                    .disabled(!canMutate || !mutationCanBeDiscarded)
            }
        case .activeRestore:
            Button("Review active version…", action: edit)
            if let discard {
                Button("Keep active version", role: .destructive, action: discard)
                    .disabled(!canMutate || !mutationCanBeDiscarded)
            }
        case .recentTrash:
            Button("Restore", action: restore).disabled(!canMutate)
        }
    }

    private var syncChip: some View {
        let presentation: (String, String, Color) = switch row.syncState {
        case .synced: ("Synced", "checkmark.circle.fill", .green)
        case .waiting: ("Queued", "clock.fill", .orange)
        case .submitted: ("Recovering", "arrow.clockwise.circle.fill", .blue)
        case .conflicted: ("Conflict", "exclamationmark.triangle.fill", .red)
        }
        return CanonicalInboxChip(
            text: presentation.0,
            symbol: presentation.1,
            color: presentation.2
        )
    }

    @ViewBuilder
    private var metadataChips: some View {
        statusChip
        privacyChip
        durationChip
        deadlineChip
    }

    private var statusChip: some View {
        CanonicalInboxChip(
            text: statusTitle,
            symbol: row.status == .inbox ? "tray" : "checkmark.circle",
            color: row.status == .inbox ? .orange : .blue
        )
    }

    private var privacyChip: some View {
        CanonicalInboxChip(
            text: row.isSensitive ? "Sensitive" : "Standard",
            symbol: row.isSensitive ? "lock.fill" : "lock.open",
            color: row.isSensitive ? .purple : .secondary
        )
    }

    private var durationChip: some View {
        CanonicalInboxChip(
            text: CanonicalItemEditorState.durationDescription(row.durationSeconds),
            symbol: "timer",
            color: .secondary
        )
    }

    @ViewBuilder
    private var deadlineChip: some View {
        if let deadline = row.deadlineAt {
            CanonicalInboxChip(
                text: deadline.formatted(date: .abbreviated, time: .shortened),
                symbol: "flag",
                color: .red
            )
        }
    }

    private var statusTitle: String {
        switch row.status {
        case .inbox: "Inbox"
        case .planned: "Planned"
        default: row.status.wireValue.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    private var mutationCanBeDiscarded: Bool {
        switch row.syncState {
        case .waiting, .conflicted: true
        case .synced, .submitted: false
        }
    }

    private var kindSymbol: String {
        switch row.kind {
        case .event: "calendar"
        case .task: "checkmark.circle"
        case .habit: "repeat"
        case .routine: "list.number"
        case .goal: "target"
        case .breakTime: "cup.and.saucer"
        case .unknown: "questionmark.diamond"
        }
    }

    private var kindColor: Color {
        switch row.kind {
        case .event: .blue
        case .task: .indigo
        case .habit: .green
        case .routine: .teal
        case .goal: .purple
        case .breakTime: .orange
        case .unknown: .secondary
        }
    }
}

private struct CanonicalInboxChip: View {
    let text: String
    let symbol: String
    let color: Color

    var body: some View {
        Label(text, systemImage: symbol)
            .font(.caption2.weight(.medium))
            .foregroundStyle(color)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(color.opacity(0.1), in: Capsule())
            .fixedSize()
    }
}
