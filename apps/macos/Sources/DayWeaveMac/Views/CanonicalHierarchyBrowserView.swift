import SwiftUI

struct CanonicalHierarchyBrowserView: View {
    @EnvironmentObject private var store: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    let scope: CanonicalHierarchyPresentation.Scope
    @State private var query = ""
    @State private var collapsedIDs: Set<UUID> = []
    @State private var editorRoute: CanonicalInboxEditorRoute?
    @State private var sourceCache = CanonicalHierarchySourceCache()
    @State private var authoringCache = CanonicalHierarchyAuthoringCache()

    var body: some View {
        let source = sourceCache.presentation(for: store)
        let presentation = CanonicalHierarchyPresentation.build(
            rows: source.hierarchyRows, scope: scope, query: query, collapsedIDs: collapsedIDs
        )
        let availability = CanonicalHierarchyAvailability.build(
            hasHydratedCache: store.canonicalDeltaCursor != nil
                && store.canonicalConfigurationIdentifier != nil,
            status: canonicalSync.status,
            hasPersistenceError: store.persistenceError != nil
        )
        let eligibleParents = authoringCache.eligibleParentIDs(for: store)
        CanonicalHierarchyBrowserContent(
            scope: scope,
            presentation: presentation,
            availability: availability,
            query: $query,
            selectedID: store.selectedCanonicalItemID,
            canMutate: store.canMutatePlan,
            timezoneName: store.scheduleProfile.timezoneName,
            select: { store.selectCanonicalItem($0) },
            toggle: { id in
                if !collapsedIDs.insert(id).inserted { collapsedIDs.remove(id) }
            },
            review: { editorRoute = CanonicalInboxEditorRoute.review(row: $0, store: store) },
            create: { editorRoute = CanonicalHierarchyAuthoring.route(kind: $0, store: store) },
            eligibleParentIDs: eligibleParents,
            addSubtask: { editorRoute = CanonicalHierarchyAuthoring.route(kind: .task, parentID: $0, store: store) }
        )
        .navigationTitle(scope.title)
        .sheet(item: $editorRoute) { route in
            CanonicalItemEditorView(
                mode: route.mode,
                readOnlyDiagnostic: route.readOnlyDiagnostic,
                profileTimezoneName: store.scheduleProfile.timezoneName
            )
            .environmentObject(store)
        }
        .onChange(of: store.canonicalConfigurationIdentifier) { _, _ in clearTransientState() }
        .onDisappear { clearTransientState() }
    }

    private func clearTransientState() {
        query = ""
        collapsedIDs = []
        editorRoute = nil
        sourceCache.clear()
        authoringCache.clear()
    }
}

/// This surface has no service/store constructors. The same content can be
/// rendered with synthetic rows and inert callbacks without launching the app.
struct CanonicalHierarchyBrowserContent: View {
    let scope: CanonicalHierarchyPresentation.Scope
    let presentation: CanonicalHierarchyPresentation
    let availability: CanonicalHierarchyAvailability
    @Binding var query: String
    let selectedID: UUID?
    let canMutate: Bool
    let timezoneName: String
    let select: (UUID) -> Void
    let toggle: (UUID) -> Void
    let review: (CanonicalInboxPresentation.Row) -> Void
    var create: (DayWeaveCanonicalItemKind) -> Void = { _ in }
    var eligibleParentIDs: Set<UUID> = []
    var addSubtask: (UUID) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 12) {
                    Image(systemName: scope.symbol)
                        .font(.title2)
                        .foregroundStyle(.tint)
                        .frame(width: 44, height: 44)
                        .background(.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
                    VStack(alignment: .leading, spacing: 4) {
                        Text(scope.title).font(.title2.weight(.semibold))
                        Text("The whole hierarchy, including work without calendar time.")
                            .font(.subheadline).foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                    Button(scope == .goals ? "New Goal" : "New Project") {
                        create(scope == .goals ? .goal : .project)
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!canMutate)
                    .accessibilityIdentifier("canonical-hierarchy.create")
                }
                HStack(spacing: 9) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Search \(scope.title.lowercased()) and their items", text: $query)
                        .textFieldStyle(.plain)
                        .privacySensitive()
                        .accessibilityLabel("Search \(scope.title.lowercased()) and their items")
                        .accessibilityIdentifier("canonical-hierarchy.\(scope.rawValue).search")
                    if !query.isEmpty {
                        Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }
                            .buttonStyle(.plain)
                            .foregroundStyle(.secondary)
                            .accessibilityLabel("Clear search")
                    }
                }
                .padding(10)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 9))
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "externaldrive.badge.checkmark")
                    Text(availability.message)
                    Spacer(minLength: 0)
                    Text(presentation.isSearching
                        ? "\(presentation.matchingItemCount) matches"
                        : "\(presentation.scopedItemCount) items")
                        .monospacedDigit()
                }
                .font(.caption).foregroundStyle(.secondary)
            }
            .padding(20)
            Divider()
            if presentation.entries.isEmpty {
                ContentUnavailableView(
                    presentation.isSearching ? "No matching items"
                        : availability.canConfirmEmpty ? "No \(scope.title.lowercased()) in this cache"
                        : "No \(scope.title.lowercased()) available yet",
                    systemImage: presentation.isSearching ? "magnifyingglass" : scope.symbol,
                    description: Text(presentation.isSearching
                        ? "Try another title. Search stays on this device."
                        : availability.canConfirmEmpty
                            ? scope == .goals
                                ? "Capture a goal in Inbox or sync existing items. Unscheduled outcomes belong here too."
                                : "Projects from your synced item collection appear here, even before their work is scheduled."
                            : "This is not proof of an empty workspace. Load or restore your items to see the complete collection.")
                )
            } else {
                ScrollView {
                    LazyVStack(spacing: 8) {
                        ForEach(presentation.entries) { entry in
                            CanonicalHierarchyBrowserRow(
                                entry: entry,
                                isSelected: selectedID == entry.id,
                                canMutate: canMutate,
                                isSearching: presentation.isSearching,
                                timezoneName: timezoneName,
                                select: { select(entry.id) },
                                toggle: { toggle(entry.id) },
                                review: { review(entry.row) },
                                canAddSubtask: eligibleParentIDs.contains(entry.id),
                                addSubtask: { addSubtask(entry.id) }
                            )
                        }
                    }
                    .padding(20)
                }
            }
            Divider()
            Label("Only schedulable leaf work adds flexible calendar time. Browsing never changes your plan.",
                  systemImage: "leaf")
                .font(.caption).foregroundStyle(.secondary)
                .padding(.horizontal, 20).padding(.vertical, 12)
        }
        .background(Color(nsColor: .windowBackgroundColor))
        .accessibilityIdentifier("canonical-hierarchy.\(scope.rawValue)")
    }
}

struct CanonicalHierarchyBrowserRow: View {
    let entry: CanonicalHierarchyPresentation.Entry
    let isSelected: Bool
    let canMutate: Bool
    let isSearching: Bool
    let timezoneName: String
    let select: () -> Void
    let toggle: () -> Void
    let review: () -> Void
    var canAddSubtask: Bool = false
    var addSubtask: () -> Void = {}

    private var row: CanonicalInboxPresentation.Row { entry.row }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Color.clear.frame(width: CGFloat(min(entry.depth, 5)) * 12)
            Button(action: toggle) {
                Image(systemName: entry.isCollapsed ? "chevron.right" : "chevron.down")
                    .font(.caption.weight(.semibold))
                    .frame(width: 20, height: 28)
            }
            .buttonStyle(.plain)
            .opacity(entry.hasChildren ? 1 : 0)
            .disabled(!entry.hasChildren || isSearching)
            .help(isSearching ? "Search reveals matching paths; clear search to change disclosure." : "Show or hide subtasks")
            .accessibilityHidden(!entry.hasChildren)
            .accessibilityLabel(entry.isCollapsed ? "Expand branch" : "Collapse branch")
            .accessibilityIdentifier("canonical-hierarchy.disclosure.\(entry.id.uuidString.lowercased())")
            Button(action: select) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 7) {
                        Image(systemName: row.kind == .project ? "folder" : row.kind == .goal ? "scope" : "circle")
                            .foregroundStyle(.tint)
                        Text(row.title).font(.headline).lineLimit(2)
                        if entry.isContext { Text("Parent context").font(.caption2).foregroundStyle(.secondary) }
                        if row.isSensitive { Image(systemName: "lock.fill").font(.caption2).foregroundStyle(.secondary) }
                    }
                    if !entry.breadcrumb.isEmpty {
                        Text(entry.breadcrumb.joined(separator: " › "))
                            .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                    Text(metadata)
                        .font(.caption).foregroundStyle(.secondary)
                    if let timing = row.timingDescription(timezoneName: timezoneName) {
                        Text(timing).font(.caption).foregroundStyle(.secondary)
                    }
                    if entry.hasUnsafeAncestry {
                        Label(row.hasHierarchyCycle ? "Hierarchy cycle · read-only"
                              : row.hasMissingParent ? "Parent unavailable · read-only"
                              : "Ancestry unavailable · read-only",
                              systemImage: "exclamationmark.triangle")
                            .font(.caption).foregroundStyle(.orange)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(row.accessibilitySummary)
            .accessibilityIdentifier("canonical-hierarchy.select.\(entry.id.uuidString.lowercased())")
            if canAddSubtask && !entry.hasUnsafeAncestry {
                Button(action: addSubtask) { Image(systemName: "plus") }
                    .buttonStyle(.borderless)
                    .disabled(!canMutate)
                    .help("Add a new task inside this item")
                    .accessibilityLabel("Add subtask")
                    .accessibilityIdentifier("canonical-hierarchy.add-subtask.\(entry.id.uuidString.lowercased())")
            }
            if !row.isReadOnly && !entry.hasUnsafeAncestry {
                Button(action: review) { Image(systemName: "square.and.pencil") }
                    .buttonStyle(.borderless)
                    .disabled(!canMutate)
                    .help("Review item details and queue supported edits")
                    .accessibilityLabel("Edit item")
                    .accessibilityIdentifier("canonical-hierarchy.edit.\(entry.id.uuidString.lowercased())")
            }
        }
        .padding(12)
        .background(isSelected ? Color.accentColor.opacity(0.12) : Color(nsColor: .controlBackgroundColor),
                    in: RoundedRectangle(cornerRadius: 11))
        .overlay {
            RoundedRectangle(cornerRadius: 11)
                .stroke(isSelected ? Color.accentColor.opacity(0.6) : .clear, lineWidth: 1)
        }
        .privacySensitive(row.isSensitive)
    }

    private var metadata: String {
        let sync: String = switch row.syncState {
        case .synced: "Synced"
        case .waiting: "Queued"
        case .submitted: "Recovering"
        case .conflicted: "Conflict · review in Inbox"
        }
        let kind: String = if case .unknown = row.kind { "Newer item type" }
            else { row.kind.wireValue.capitalized }
        let status: String = if case .unknown = row.status { "Newer lifecycle state" }
            else { row.status.wireValue.replacingOccurrences(of: "_", with: " ").capitalized }
        return "\(kind) · \(status) · \(sync) · \(row.durationDescription) · Level \(entry.depth + 1)"
    }
}
