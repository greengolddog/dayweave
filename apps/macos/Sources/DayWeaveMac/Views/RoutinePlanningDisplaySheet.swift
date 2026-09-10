import SwiftUI

/// The only action is Close. Inert display rows never enter the active
/// schedule's selection, gestures, context menu, mutation or execution routes.
struct RoutinePlanningDisplaySheet: View {
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var canonicalSync: CanonicalSyncStore
    @EnvironmentObject private var appLock: AppLockController
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Label("Saved routine preview", systemImage: "lock.doc")
                    .font(.title2.weight(.semibold))
                Spacer()
                Button("Close") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            if appLock.isContentAvailable, planner.canPersistPlan,
               let preview = canonicalSync.routinePlanningDisplayPresentation {
                Label("Read-only · execution and publication locked", systemImage: "lock.fill")
                    .font(.callout.weight(.medium))
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Original planning clock: \(preview.asOf)")
                    Text("Fixed horizon: \(preview.horizonStart) – \(preview.horizonEnd)")
                    Text("Planning timezone: \(preview.timezoneName)")
                }
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                Text("\(preview.scheduledMinutes) min scheduled · \(preview.unscheduledMinutes) min unscheduled · \(preview.instanceCount) routine instances · retained lifecycle head \(preview.occurrenceSnapshotRevision)")
                    .font(.caption)
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        if preview.blocks.isEmpty {
                            Text("No blocks were produced for this original fixed input.")
                                .foregroundStyle(.secondary)
                                .padding(.vertical)
                        }
                        ForEach(preview.blocks) { block in
                            VStack(alignment: .leading, spacing: 5) {
                                Text(block.title).font(.headline)
                                Text("\(format(block.start, timezone: preview.timezoneName)) – \(format(block.end, timezone: preview.timezoneName))")
                                    .font(.caption.monospacedDigit())
                                    .foregroundStyle(.secondary)
                                if !block.explanation.isEmpty {
                                    Text(block.explanation).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(12)
                            .background(Color.accentColor.opacity(0.06), in: RoundedRectangle(cornerRadius: 10))
                        }
                        if !preview.unscheduled.isEmpty {
                            Text("Unscheduled in this fixed input").font(.headline).padding(.top, 8)
                            ForEach(preview.unscheduled) { row in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("\(row.title) · \(row.remainingMinutes) min remaining").font(.subheadline.weight(.medium))
                                    Text(row.message).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                        }
                        if !preview.notices.isEmpty {
                            DisclosureGroup("Planning explanations") {
                                ForEach(Array(preview.notices.enumerated()), id: \.offset) { _, notice in
                                    Text(notice).font(.caption).foregroundStyle(.secondary)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                }
                            }
                        }
                        if !preview.members.isEmpty {
                            DisclosureGroup("Retained member states · not live history") {
                                LazyVStack(alignment: .leading, spacing: 7) {
                                    ForEach(preview.members) { member in
                                        HStack {
                                            Text(member.title)
                                            Spacer()
                                            Text(member.status.rawValue.replacingOccurrences(of: "_", with: " "))
                                                .foregroundStyle(.secondary)
                                        }.font(.caption)
                                    }
                                }
                            }
                        }
                    }
                }
                Text("This preview uses the original saved inputs. It does not advance the planning clock or change the active schedule, occurrence history, or pending recovery.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                ContentUnavailableView("Preview is locked or stale", systemImage: "lock.fill",
                    description: Text("Unlock DayWeave and explicitly recompute the saved input before viewing. If its sources or planning day changed, prepare a new input while connected."))
            }
        }
        .padding(22)
        .frame(minWidth: 600, idealWidth: 740, minHeight: 520, idealHeight: 690)
        .privacySensitive()
        .accessibilityIdentifier("routine-planning.display-sheet")
        .task {
            while !Task.isCancelled {
                canonicalSync.refreshRoutinePlanningDisplayAdmission()
                do { try await Task.sleep(for: .seconds(1)) }
                catch { return }
            }
        }
    }

    private func format(_ date: Date, timezone: String) -> String {
        let formatter = DateFormatter()
        formatter.timeZone = TimeZone(identifier: timezone)
        formatter.dateStyle = .medium; formatter.timeStyle = .short
        return formatter.string(from: date)
    }
}
