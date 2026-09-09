import SwiftUI

struct ItemProgressOutboxView: View {
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var progress: ItemProgressStore
    @State private var message: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if planner.itemProgressState.journals.isEmpty {
                Text("No saved progress requests await recovery.").font(.caption).foregroundStyle(.secondary)
            }
            ForEach(progress.recoveryEntries) { entry in
                VStack(alignment: .leading, spacing: 5) {
                    Text("Item \(entry.itemID.uuidString.prefix(8))").font(.subheadline.weight(.medium))
                    Text(entry.isRejected ? "Not applied · review or discard retained intent" : "Exact request retained for recovery")
                        .font(.caption)
                    if entry.canDiscard {
                        Button("Discard saved intent") {
                            do { try progress.discardReviewedIntent(entry.itemID, expectedOperationID: entry.id) }
                            catch { message = "The request remains retained; it could not be safely discarded." }
                        }.disabled(progress.isWorking)
                    }
                }
            }
            Button("Recover exact progress requests") { Task { await progress.replayPending() } }
                .disabled(progress.isWorking || !progress.hasPendingRecovery)
            if let message { Text(message).font(.caption).foregroundStyle(.secondary) }
        }
    }
}

struct ItemProgressPanel: View {
    @EnvironmentObject private var planner: PlannerStore
    @EnvironmentObject private var progress: ItemProgressStore
    let itemID: UUID
    let sensitive: Bool
    @State private var review: ItemProgressReview?
    @State private var localMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Independent progress").font(.headline)
            Text("Your own records—not child totals, execution credit, or item completion.")
                .font(.caption).foregroundStyle(.secondary)
            if let observation = progress.observation(for: itemID) {
                Text(observation.isReadProof ? "Saved GET observation · revision \(observation.snapshot.revision)"
                     : "Historical operation confirmed · refresh before editing")
                    .font(.caption).foregroundStyle(.secondary)
                if observation.snapshot.components.isEmpty {
                    Text("No independent components recorded.").font(.caption)
                }
                ForEach(observation.snapshot.components) { component in
                    VStack(alignment: .leading, spacing: 3) {
                        Text(component.name).font(.subheadline.weight(.medium))
                        Text(component.value.description).font(.caption).foregroundStyle(.secondary)
                    }.privacySensitive(sensitive || progress.isSensitive(itemID))
                }
            } else {
                Text("A verified item-scoped GET is needed. Unavailable progress is not empty progress.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let journal = progress.journal(for: itemID) {
                Divider()
                Text(journal.noEffectCode == nil
                    ? journal.hasBeenSubmitted ? "Exact request awaiting recovery" : "Reviewed values queued offline"
                    : "Not applied · saved values need a new review")
                    .font(.caption.weight(.medium))
                ForEach(journal.command.components) { component in
                    Text("\(component.name): \(component.value.description)")
                        .font(.caption).privacySensitive(sensitive || progress.isSensitive(itemID))
                }
                if journal.command.components.isEmpty { Text("Queued: clear independent components.").font(.caption) }
                if !journal.hasBeenSubmitted || journal.noEffectCode != nil {
                    Button("Discard saved intent") {
                        do { try progress.discardReviewedIntent(itemID, expectedOperationID: journal.id) }
                        catch { localMessage = "The saved intent could not be cleared safely." }
                    }.disabled(progress.isWorking)
                }
            }
            HStack {
                Button("Refresh") { Task { await progress.refresh(itemID) } }.disabled(progress.isWorking)
                Button(progress.journal(for: itemID) == nil ? "Review progress…" : "Review saved values…") {
                    guard let baseline = progress.observation(for: itemID)?.snapshot else { return }
                    review = .init(baseline: baseline,
                        components: progress.journal(for: itemID)?.command.components ?? baseline.components,
                        sensitive: sensitive || progress.isSensitive(itemID))
                }.disabled(!progress.canReview(itemID))
            }
            Text(localMessage ?? progress.message).font(.caption).foregroundStyle(.secondary)
        }
        .accessibilityIdentifier("item-progress.panel")
        .onAppear { progress.showDetail(itemID) }
        .onChange(of: itemID) { _, value in review = nil; progress.showDetail(value) }
        .onChange(of: planner.canonicalConfigurationIdentifier) { _, _ in review = nil; progress.hideDetail() }
        .onDisappear { review = nil; progress.hideDetail() }
        .sheet(item: $review) { context in
            ItemProgressReviewView(context: context) { values in
                try progress.queue(itemID: itemID, baseline: context.baseline, components: values)
            }.privacySensitive(context.sensitive || progress.isSensitive(itemID))
        }
    }
}

struct ItemProgressReview: Identifiable {
    let id = UUID()
    let baseline: ItemProgressSnapshot
    let components: [ItemProgressComponent]
    let sensitive: Bool
}

struct ItemProgressEditorComponent: Identifiable, Equatable {
    enum Kind: String, CaseIterable, Identifiable { case percentage, time, quantity; var id: String { rawValue } }
    let id: UUID
    var name: String
    var kind: Kind
    var percentage = "0"
    var elapsed = "0"
    var remaining = ""
    var current = "0"
    var unit = "units"
    var target = ""
    var direction = ItemProgressTarget.Direction.atLeast

    init(component: ItemProgressComponent) {
        id = component.id; name = component.name
        switch component.value {
        case let .percentage(points):
            kind = .percentage; percentage = "\(points / 100).\(String(format: "%02d", points % 100))"
        case let .time(seconds, left):
            kind = .time; elapsed = String(seconds); remaining = left.map(String.init) ?? ""
        case let .quantity(value, units, goal):
            kind = .quantity; current = value; unit = units; target = goal?.value ?? ""; direction = goal?.direction ?? .atLeast
        }
    }
    var component: ItemProgressComponent? {
        let value: ItemProgressValue
        switch kind {
        case .percentage:
            guard percentage.range(of: #"^[0-9]{1,3}(\.[0-9]{1,2})?$"#, options: .regularExpression) != nil else { return nil }
            let parts = percentage.split(separator: ".")
            guard let integer = UInt16(parts[0]) else { return nil }
            let fraction = parts.count == 2 ? String(parts[1]).padding(toLength: 2, withPad: "0", startingAt: 0) : "00"
            guard let hundredths = UInt16(fraction), integer <= 100 else { return nil }
            value = .percentage(basisPoints: integer * 100 + hundredths)
        case .time:
            guard let seconds = UInt64(elapsed), elapsed.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
            let left = remaining.isEmpty ? nil : UInt64(remaining)
            guard remaining.isEmpty || (left != nil && remaining.allSatisfy { $0.isASCII && $0.isNumber }) else { return nil }
            value = .time(elapsedSeconds: seconds, remainingSeconds: left)
        case .quantity:
            guard let normalized = ItemProgressValidation.normalizedInputDecimal(current),
                  target.isEmpty || ItemProgressValidation.normalizedInputDecimal(target) != nil else { return nil }
            value = .quantity(current: normalized, unit: unit,
                target: target.isEmpty ? nil : .init(value: ItemProgressValidation.normalizedInputDecimal(target)!, direction: direction))
        }
        let result = ItemProgressComponent(id: id, name: name, value: value)
        return result.isValid ? result : nil
    }
}

/// Pure review surface with injectable save; synthetic rendering never creates
/// a production app, network client, credential store, or provider service.
struct ItemProgressReviewView: View {
    @Environment(\.dismiss) private var dismiss
    let context: ItemProgressReview
    let save: ([ItemProgressComponent]) throws -> Void
    @State private var fields: [ItemProgressEditorComponent]
    @State private var message: String?
    init(context: ItemProgressReview, save: @escaping ([ItemProgressComponent]) throws -> Void) {
        self.context = context; self.save = save
        _fields = State(initialValue: context.components.map(ItemProgressEditorComponent.init))
    }
    private var values: [ItemProgressComponent]? {
        let result = fields.compactMap(\.component)
        return result.count == fields.count && ItemProgressValidation.components(result) ? result : nil
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Review independent progress").font(.title2.weight(.semibold))
            Text("This saves your own records only. It does not complete the item, change calendar time, or add execution credit.")
                .font(.subheadline).foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ForEach($fields) { $field in
                        VStack(alignment: .leading, spacing: 10) {
                            HStack(alignment: .bottom) {
                                editorField("Component name", text: $field.name)
                                Picker("Type", selection: $field.kind) {
                                    ForEach(ItemProgressEditorComponent.Kind.allCases) { kind in
                                        Text(kind.rawValue.capitalized).tag(kind)
                                    }
                                }.frame(width: 230)
                                Button { fields.removeAll { $0.id == field.id } } label: { Image(systemName: "minus.circle") }
                                    .accessibilityLabel("Remove component")
                            }
                            switch field.kind {
                            case .percentage:
                                editorField("Percentage (0–100, up to two decimals)", text: $field.percentage)
                            case .time:
                                HStack(alignment: .bottom) {
                                    editorField("Elapsed seconds", text: $field.elapsed)
                                    editorField("Remaining seconds (blank if unknown)", text: $field.remaining)
                                }
                            case .quantity:
                                HStack(alignment: .bottom) {
                                    editorField("Current exact value", text: $field.current)
                                    editorField("Unit", text: $field.unit)
                                }
                                HStack(alignment: .bottom) {
                                    editorField("Target (optional)", text: $field.target)
                                    Picker("Direction", selection: $field.direction) {
                                        Text("At least").tag(ItemProgressTarget.Direction.atLeast)
                                        Text("At most").tag(ItemProgressTarget.Direction.atMost)
                                    }.frame(width: 230)
                                }
                            }
                        }.padding(14).background(.quaternary, in: RoundedRectangle(cornerRadius: 10))
                    }
                    Button("Add component") {
                        fields.append(.init(component: .init(name: "New component", value: .percentage(basisPoints: 0))))
                    }.disabled(fields.count >= 16)
                    Text("Up to 16 components. Quantity values are exact plain decimals, with up to six decimal places. Extra trailing zeros are normalized. Blank remaining time or target means unknown—not zero.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            if values == nil { Text("Check names, units and exact numeric values before saving.").font(.caption).foregroundStyle(.orange) }
            if let message { Text(message).font(.caption).foregroundStyle(.orange) }
            HStack {
                Button("Cancel") { dismiss() }
                Spacer()
                Text("Item revision \(context.baseline.itemRevision) · progress revision \(context.baseline.revision)")
                    .font(.caption).foregroundStyle(.secondary)
                Button("Save reviewed progress") {
                    guard let values else { return }
                    do { try save(values); dismiss() }
                    catch { message = (error as? ItemProgressError)?.errorDescription ?? "The reviewed change could not be saved safely." }
                }.buttonStyle(.borderedProminent).disabled(values == nil)
            }
        }
        .padding(24).frame(width: 740, height: 680)
        .textFieldStyle(.roundedBorder)
        .privacySensitive(context.sensitive)
        .accessibilityIdentifier("item-progress.review")
    }

    private func editorField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField(title, text: text).accessibilityLabel(title)
        }
    }
}
