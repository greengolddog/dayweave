import SwiftUI

struct CalendarDestinationView: View {
    @EnvironmentObject private var store: PlannerStore
    @State private var selectedDate = Calendar.autoupdatingCurrent.startOfDay(for: Date())

    private let calendar = Calendar.autoupdatingCurrent

    private var weekDays: [Date] {
        let interval = calendar.dateInterval(of: .weekOfYear, for: selectedDate)
        let start = interval?.start ?? calendar.startOfDay(for: selectedDate)
        return (0..<7).compactMap { calendar.date(byAdding: .day, value: $0, to: start) }
    }

    private var selectedBlocks: [ScheduleBlock] {
        let start = calendar.startOfDay(for: selectedDate)
        let end = calendar.date(byAdding: .day, value: 1, to: start)
            ?? start.addingTimeInterval(86_400)
        return store.blocks
            .filter { $0.end > start && $0.start < end }
            .sorted { $0.start < $1.start }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Calendar").font(.title2.weight(.semibold))
                    Text("Firm blocks publish to Google after an account and writable calendar are connected.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button {
                    selectedDate = calendar.date(byAdding: .weekOfYear, value: -1, to: selectedDate) ?? selectedDate
                } label: {
                    Image(systemName: "chevron.left")
                }
                Button("Today") {
                    selectedDate = calendar.startOfDay(for: Date())
                }
                Button {
                    selectedDate = calendar.date(byAdding: .weekOfYear, value: 1, to: selectedDate) ?? selectedDate
                } label: {
                    Image(systemName: "chevron.right")
                }
            }
            .padding(20)

            HStack(spacing: 8) {
                ForEach(weekDays, id: \.self) { date in
                    Button {
                        selectedDate = date
                    } label: {
                        VStack(spacing: 5) {
                            Text(date.formatted(.dateTime.weekday(.abbreviated)))
                                .font(.caption)
                            Text(date.formatted(.dateTime.day()))
                                .font(.headline)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 9)
                        .background(
                            calendar.isDate(date, inSameDayAs: selectedDate)
                                ? Color.accentColor.opacity(0.16)
                                : Color.clear,
                            in: RoundedRectangle(cornerRadius: 10)
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 20)
            .padding(.bottom, 14)
            Divider()

            if selectedBlocks.isEmpty {
                ContentUnavailableView(
                    "No blocks on this day",
                    systemImage: "calendar.badge.plus",
                    description: Text("Capture work or navigate to a day that already has a plan.")
                )
            } else {
                ScrollView {
                    LazyVStack(spacing: 10) {
                        ForEach(selectedBlocks) { block in
                            CalendarAgendaRow(block: block)
                                .onTapGesture { store.select(block) }
                        }
                    }
                    .padding(20)
                }
            }
        }
        .navigationTitle("Calendar")
    }
}

private struct CalendarAgendaRow: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock

    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .trailing, spacing: 3) {
                Text(block.startTimeLabel)
                    .font(.system(.subheadline, design: .monospaced).weight(.semibold))
                Text(block.endTimeLabel)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(width: 108)
            RoundedRectangle(cornerRadius: 3)
                .fill(block.kind.color)
                .frame(width: 5, height: 48)
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(block.title).font(.headline)
                    if block.isHardConstraint {
                        Image(systemName: "lock.fill").font(.caption2).foregroundStyle(.secondary)
                    }
                }
                Text(block.project ?? block.kind.title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Text(block.status.title)
                .font(.caption.weight(.medium))
                .foregroundStyle(block.status == .active ? .green : .secondary)
        }
        .padding(14)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 13))
        .overlay {
            RoundedRectangle(cornerRadius: 13)
                .stroke(store.selectedBlockID == block.id ? Color.accentColor : .clear, lineWidth: 1)
        }
    }
}

struct HabitsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore

    private var habits: [ScheduleBlock] {
        let calendar = Calendar.autoupdatingCurrent
        let start = calendar.startOfDay(for: Date())
        let end = calendar.date(byAdding: .day, value: 1, to: start)
            ?? start.addingTimeInterval(86_400)
        return store.blocks
            .filter { $0.kind == .habit && $0.end > start && $0.start < end }
            .sorted { $0.start < $1.start }
    }

    var body: some View {
        DestinationScroll(title: "Habits", subtitle: "Occurrences stay distinct from tasks and can be completed, skipped, or moved.") {
            SummaryStrip(metrics: [
                ("\(habits.count)", "today", "repeat"),
                ("\(habits.count(where: { $0.status == .completed }))", "completed", "checkmark.circle"),
                ("\(habits.reduce(0) { $0 + $1.durationMinutes })m", "planned", "clock"),
            ])

            if habits.isEmpty {
                DestinationEmpty(title: "No habit occurrences", symbol: "repeat.circle", action: "Add habit") {
                    store.isQuickAddPresented = true
                }
            } else {
                ForEach(habits) { habit in
                    PlanningCard(block: habit, detail: habit.notes) {
                        Button("Complete") { store.complete(habit.id) }
                            .disabled(!store.canMutate(habit))
                        Button("Skipped") { store.skip(habit.id) }
                            .disabled(!store.canMutate(habit))
                        Button("Will do later") { store.doLater(habit.id) }
                            .disabled(
                                !store.canMutate(habit)
                                    || !habit.isFlexible
                                    || habit.isHardConstraint
                            )
                    }
                }
            }
        }
        .navigationTitle("Habits")
    }
}

struct ProjectsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore

    private var groups: [(String, [ScheduleBlock])] {
        Dictionary(grouping: store.blocks) { $0.project ?? "Personal" }
            .map { ($0.key, $0.value.sorted { $0.start < $1.start }) }
            .sorted { $0.0.localizedCaseInsensitiveCompare($1.0) == .orderedAscending }
    }

    var body: some View {
        DestinationScroll(title: "Projects", subtitle: "Project progress rolls up from executable leaf work.") {
            ForEach(groups, id: \.0) { name, blocks in
                let completed = blocks.count(where: { $0.status == .completed })
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Image(systemName: "folder.fill").foregroundStyle(.blue)
                        Text(name).font(.title3.weight(.semibold))
                        Spacer()
                        Text("\(completed)/\(blocks.count) done")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    ProgressView(value: Double(completed), total: Double(max(blocks.count, 1)))
                    ForEach(blocks.prefix(4)) { block in
                        Button {
                            store.select(block)
                        } label: {
                            HStack {
                                Image(systemName: block.status == .completed ? "checkmark.circle.fill" : block.kind.symbol)
                                    .foregroundStyle(block.status == .completed ? .green : block.kind.color)
                                Text(block.title).lineLimit(1)
                                Spacer()
                                Text(block.timeRange).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(16)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            }
        }
        .navigationTitle("Projects")
    }
}

struct GoalsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore

    private var goals: [ScheduleBlock] {
        store.blocks.filter { $0.kind == .goal }.sorted { $0.start < $1.start }
    }

    var body: some View {
        DestinationScroll(title: "Goals", subtitle: "Outcomes can remain unscheduled; only their executable leaf actions reserve time.") {
            if goals.isEmpty {
                DestinationEmpty(title: "No goals yet", symbol: "scope", action: "Capture a goal") {
                    store.isQuickAddPresented = true
                }
            } else {
                ForEach(goals) { goal in
                    PlanningCard(block: goal, detail: goal.notes) {
                        Button("Open") { store.select(goal) }
                        Button("Complete") { store.complete(goal.id) }
                            .disabled(!store.canMutate(goal))
                    }
                }
            }

            VStack(alignment: .leading, spacing: 10) {
                Label("Goal scheduling rule", systemImage: "info.circle")
                    .font(.headline)
                Text("A goal without a duration is an outcome. Add leaf tasks, milestones, routines, or habits to turn it into calendar demand.")
                    .foregroundStyle(.secondary)
            }
            .padding(16)
            .background(Color.purple.opacity(0.08), in: RoundedRectangle(cornerRadius: 14))
        }
        .navigationTitle("Goals")
    }
}

struct StatisticsDestinationView: View {
    @EnvironmentObject private var store: PlannerStore

    private var plannedMinutes: Int { store.blocks.reduce(0) { $0 + $1.durationMinutes } }
    private var actualMinutes: Int {
        store.blocks.filter { $0.status == .completed }.reduce(0) { $0 + ($1.actualMinutes ?? $1.durationMinutes) }
    }

    var body: some View {
        DestinationScroll(title: "Statistics", subtitle: "Today’s transparent execution summary. Historical trends appear as real data accumulates.") {
            SummaryStrip(metrics: [
                ("\(plannedMinutes)m", "planned", "calendar"),
                ("\(actualMinutes)m", "completed", "timer"),
                ("\(store.completedCount)", "items done", "checkmark.circle"),
            ])

            StatisticsSection(title: "Status") {
                ForEach(PlannerItemStatus.allCases, id: \.self) { status in
                    let count = store.blocks.count(where: { $0.status == status })
                    if count > 0 {
                        DistributionRow(
                            label: status.title,
                            value: count,
                            total: max(store.blocks.count, 1),
                            color: status == .completed ? .green : .accentColor
                        )
                    }
                }
            }

            StatisticsSection(title: "Energy demand") {
                ForEach(EnergyLevel.allCases) { energy in
                    let minutes = store.blocks
                        .filter { $0.energy == energy }
                        .reduce(0) { $0 + $1.durationMinutes }
                    DistributionRow(
                        label: energy.title,
                        value: minutes,
                        total: max(plannedMinutes, 1),
                        valueSuffix: "m",
                        color: energy == .deep ? .indigo : energy == .medium ? .orange : .mint
                    )
                }
            }
        }
        .navigationTitle("Statistics")
    }
}

private struct DestinationScroll<Content: View>: View {
    let title: String
    let subtitle: String
    @ViewBuilder let content: Content

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title).font(.title2.weight(.semibold))
                    Text(subtitle).font(.subheadline).foregroundStyle(.secondary)
                }
                .padding(.bottom, 4)
                content
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(20)
        }
    }
}

private struct SummaryStrip: View {
    let metrics: [(String, String, String)]

    var body: some View {
        HStack(spacing: 10) {
            ForEach(Array(metrics.enumerated()), id: \.offset) { _, metric in
                HStack(spacing: 9) {
                    Image(systemName: metric.2).foregroundStyle(.tint)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(metric.0).font(.headline)
                        Text(metric.1).font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 0)
                }
                .padding(13)
                .frame(maxWidth: .infinity)
                .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
            }
        }
    }
}

private struct PlanningCard<Actions: View>: View {
    @EnvironmentObject private var store: PlannerStore
    let block: ScheduleBlock
    let detail: String
    @ViewBuilder let actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Image(systemName: block.kind.symbol).foregroundStyle(block.kind.color)
                Text(block.title).font(.headline)
                Spacer()
                Text(block.status.title).font(.caption).foregroundStyle(.secondary)
            }
            HStack(spacing: 12) {
                Label(block.timeRange, systemImage: "clock")
                Label("\(block.durationMinutes)m", systemImage: "timer")
                Label(block.energy.title, systemImage: "bolt")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            if !detail.isEmpty {
                Text(detail).font(.subheadline).foregroundStyle(.secondary)
            }
            HStack { actions }
                .controlSize(.small)
        }
        .padding(16)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
        .onTapGesture { store.select(block) }
    }
}

private struct DestinationEmpty: View {
    let title: String
    let symbol: String
    let action: String
    let perform: () -> Void

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: symbol).font(.largeTitle).foregroundStyle(.secondary)
            Text(title).font(.headline)
            Button(action, action: perform).buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity)
        .padding(36)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
    }
}

private struct StatisticsSection<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title).font(.headline)
            content
        }
        .padding(16)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
    }
}

private struct DistributionRow: View {
    let label: String
    let value: Int
    let total: Int
    var valueSuffix = ""
    let color: Color

    var body: some View {
        HStack(spacing: 12) {
            Text(label).frame(width: 100, alignment: .leading)
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule().fill(.quaternary)
                    Capsule()
                        .fill(color)
                        .frame(width: geometry.size.width * min(1, Double(value) / Double(max(total, 1))))
                }
            }
            .frame(height: 8)
            Text("\(value)\(valueSuffix)")
                .font(.system(.caption, design: .monospaced))
                .frame(width: 54, alignment: .trailing)
        }
    }
}
