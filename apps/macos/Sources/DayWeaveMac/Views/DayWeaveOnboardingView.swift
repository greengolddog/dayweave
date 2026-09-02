import SwiftUI

struct DayWeaveOnboardingActions {
    var openAPIConnection: () -> Void
    var openGoogleResources: () -> Void
    var openScheduleProfile: () -> Void
    var configureNotifications: () -> Void
    var captureFirstItem: () -> Void
    var composeFirstPlan: () -> Void
    var dismiss: () -> Void
    var didComplete: () -> Void

    init(
        openAPIConnection: @escaping () -> Void = {},
        openGoogleResources: @escaping () -> Void = {},
        openScheduleProfile: @escaping () -> Void = {},
        configureNotifications: @escaping () -> Void = {},
        captureFirstItem: @escaping () -> Void = {},
        composeFirstPlan: @escaping () -> Void = {},
        dismiss: @escaping () -> Void = {},
        didComplete: @escaping () -> Void = {}
    ) {
        self.openAPIConnection = openAPIConnection
        self.openGoogleResources = openGoogleResources
        self.openScheduleProfile = openScheduleProfile
        self.configureNotifications = configureNotifications
        self.captureFirstItem = captureFirstItem
        self.composeFirstPlan = composeFirstPlan
        self.dismiss = dismiss
        self.didComplete = didComplete
    }
}

struct DayWeaveOnboardingView: View {
    @ObservedObject private var controller: DayWeaveOnboardingController
    private let readiness: DayWeaveOnboardingReadiness
    private let actions: DayWeaveOnboardingActions
    @State private var showsProgressResetConfirmation = false

    init(
        controller: DayWeaveOnboardingController,
        readiness: DayWeaveOnboardingReadiness,
        actions: DayWeaveOnboardingActions
    ) {
        self.controller = controller
        self.readiness = readiness
        self.actions = actions
    }

    var body: some View {
        HStack(spacing: 0) {
            progressSidebar
                .frame(width: 238)
            Divider()
            VStack(spacing: 0) {
                header
                Divider()
                ScrollView {
                    stepContent
                        .frame(maxWidth: 680, alignment: .leading)
                        .padding(.horizontal, 44)
                        .padding(.vertical, 34)
                        .frame(maxWidth: .infinity, alignment: .top)
                }
                Divider()
                footer
            }
        }
        .frame(minWidth: 900, minHeight: 650)
        .background(Color(nsColor: .windowBackgroundColor))
        .onChange(of: controller.currentStep) { _, step in
            dayWeavePostAccessibilityAnnouncement(
                "Setup step \(step.ordinal + 1) of \(DayWeaveOnboardingStep.allCases.count): \(step.title)."
            )
        }
        .confirmationDialog(
            "Reset only guided-setup progress?",
            isPresented: $showsProgressResetConfirmation,
            titleVisibility: .visible
        ) {
            Button("Reset setup progress", role: .destructive) {
                controller.resetProgressAfterWarning()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This replaces the unreadable setup checkpoint. It does not remove planner data, accounts, credentials, Google recovery, or schedules.")
        }
        .accessibilityIdentifier("onboarding.flow")
    }

    private var progressSidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 7) {
                Label("DayWeave", systemImage: "sparkles")
                    .font(.title2.weight(.semibold))
                Text("Set up your private planning workspace")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 20)
            .padding(.top, 24)
            .padding(.bottom, 20)

            VStack(spacing: 4) {
                ForEach(DayWeaveOnboardingStep.allCases) { step in
                    Button {
                        controller.navigate(to: step)
                    } label: {
                        HStack(spacing: 11) {
                            stepMarker(step)
                                .frame(width: 24)
                            Text(step.title)
                                .font(.subheadline.weight(
                                    controller.currentStep == step ? .semibold : .regular
                                ))
                                .foregroundStyle(stepForeground(step))
                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 9)
                        .contentShape(Rectangle())
                        .background(
                            controller.currentStep == step
                                ? Color.accentColor.opacity(0.13)
                                : Color.clear,
                            in: RoundedRectangle(cornerRadius: 9)
                        )
                    }
                    .buttonStyle(.plain)
                    .disabled(!controller.canNavigate(to: step))
                    .accessibilityLabel(step.title)
                    .accessibilityValue(stepAccessibilityValue(step))
                    .accessibilityIdentifier("onboarding.sidebar.\(step.rawValue)")
                }
            }
            .padding(.horizontal, 10)

            Spacer()

            VStack(alignment: .leading, spacing: 8) {
                ProgressView(
                    value: Double(controller.currentStep.ordinal + 1),
                    total: Double(DayWeaveOnboardingStep.allCases.count)
                )
                Text("Step \(controller.currentStep.ordinal + 1) of \(DayWeaveOnboardingStep.allCases.count)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .padding(20)
            .accessibilityElement(children: .combine)
            .accessibilityLabel(
                "Onboarding progress, step \(controller.currentStep.ordinal + 1) of \(DayWeaveOnboardingStep.allCases.count)"
            )
        }
        .background(Color(nsColor: .controlBackgroundColor).opacity(0.58))
    }

    private var header: some View {
        HStack(spacing: 14) {
            ZStack {
                RoundedRectangle(cornerRadius: 12)
                    .fill(Color.accentColor.opacity(0.14))
                Image(systemName: controller.currentStep.symbol)
                    .font(.title2.weight(.semibold))
                    .foregroundStyle(.tint)
            }
            .frame(width: 46, height: 46)
            .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 3) {
                Text(controller.currentStep.title)
                    .font(.title2.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text(stepSubtitle)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 26)
        .padding(.vertical, 17)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("onboarding.header.\(controller.currentStep.rawValue)")
    }

    @ViewBuilder
    private var stepContent: some View {
        switch controller.currentStep {
        case .welcomePrivacy:
            welcomePrivacyPage
        case .apiConnection:
            actionPage(
                eyebrow: "PRIVATE BACKEND",
                title: "Connect this Mac",
                explanation: "Your DayWeave session synchronizes canonical items and execution state. Google and ChatGPT identities remain separate.",
                check: readiness.apiConnection,
                buttonTitle: readiness.apiConnection.isReady
                    ? "Review API connection" : "Connect or verify DayWeave API",
                buttonSymbol: "network",
                action: actions.openAPIConnection,
                note: "Credentials are handled by the existing secure enrollment flow. Onboarding never reads or stores them."
            )
        case .googleResources:
            actionPage(
                eyebrow: "CALENDAR & TASKS",
                title: "Choose what DayWeave may use",
                explanation: "Connect Google, then select the calendars and task lists that belong in your plan. Calendar publishing remains a separate explicit approval.",
                check: readiness.googleResources,
                buttonTitle: readiness.googleResources.isReady
                    ? "Review Google resources" : "Choose Google resources",
                buttonSymbol: "calendar.badge.checkmark",
                action: actions.openGoogleResources,
                note: "Provider tokens stay on the server. This page receives only a readiness result from the integration layer."
            )
        case .scheduleProfile:
            actionPage(
                eyebrow: "YOUR WEEK",
                title: "Review the shape of your time",
                explanation: "DayWeave starts with a complete local profile. Review its time zone, sleep, availability, protected time, energy, contexts, and location; continuing confirms the current values.",
                check: readiness.scheduleProfile,
                buttonTitle: "Review schedule profile",
                buttonSymbol: "calendar.badge.clock",
                action: actions.openScheduleProfile,
                note: "The scheduler treats sleep and protected time as visible constraints, not empty gaps."
            )
        case .notifications:
            actionPage(
                eyebrow: "INTERRUPTIONS WITH PURPOSE",
                title: "Review how DayWeave may reach you",
                explanation: "This step is informational until a reminder is needed. If permission is undecided, continuing keeps it deferred; DayWeave asks only after you explicitly create a future timed break.",
                check: readiness.notifications,
                buttonTitle: "Open macOS notification settings",
                buttonSymbol: "bell.badge",
                action: actions.configureNotifications,
                note: "Onboarding never invents a break to trigger permission. Sensitive notification text remains redacted."
            )
        case .firstItem:
            actionPage(
                eyebrow: "CAPTURE",
                title: "Give DayWeave something real to plan",
                explanation: "Create one reviewed Planned leaf item with a duration, or a fully timed event, so the first composition has real demand.",
                check: readiness.firstItem,
                buttonTitle: readiness.firstItem.state == .pending
                    ? "Create first item" : "Review first item",
                buttonSymbol: "square.and.pencil",
                action: actions.captureFirstItem,
                note: "Item content and the opaque first-item checkpoint remain together in the encrypted planner store; guided-setup preferences never retain the title or constraints."
            )
        case .firstPlan:
            actionPage(
                eyebrow: "COMPOSE",
                title: "Turn constraints into a day",
                explanation: "Sync the reviewed item, compose the deterministic seven-day plan, and durably publish that exact revision to DayWeave.",
                check: readiness.firstPlan,
                buttonTitle: readiness.firstPlan.isReady
                    ? "Review first plan" : "Compose first plan",
                buttonSymbol: "wand.and.stars",
                action: actions.composeFirstPlan,
                note: "This explicit action publishes the internal schedule revision. Google Calendar still follows the source roles and approval rules you selected."
            )
        case .completion:
            completionPage
        }
    }

    private var welcomePrivacyPage: some View {
        VStack(alignment: .leading, spacing: 24) {
            VStack(alignment: .leading, spacing: 9) {
                Text("A calmer, executable day")
                    .font(.largeTitle.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text("This short setup connects the pieces DayWeave needs while keeping every external effect visible and reviewable.")
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            VStack(spacing: 12) {
                onboardingFeature(
                    symbol: "lock.shield.fill",
                    title: "Private by default",
                    detail: "Planner data is encrypted locally. Sensitive items are excluded from external assistant context and lock-screen detail by default."
                )
                onboardingFeature(
                    symbol: "arrow.triangle.2.circlepath",
                    title: "Useful offline",
                    detail: "Capture, viewing, execution, and local composition continue without Google, ChatGPT, or the network."
                )
                onboardingFeature(
                    symbol: "checkmark.seal",
                    title: "You approve external effects",
                    detail: "Publishing, destructive changes, and relaxed hard constraints cross an explicit review boundary."
                )
            }

            Toggle(isOn: Binding(
                get: { controller.progress.privacyAcknowledged },
                set: { controller.setPrivacyAcknowledged($0) }
            )) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("I understand the privacy and approval boundaries")
                        .font(.headline)
                    Text("You can revisit privacy and app-lock settings later.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .toggleStyle(.switch)
            .padding(16)
            .background(Color.accentColor.opacity(0.09), in: RoundedRectangle(cornerRadius: 13))
            .disabled(controller.progress.furthestReachedStep != .welcomePrivacy)
            .accessibilityLabel("Acknowledge the privacy and approval boundaries")
            .accessibilityHint("Required before continuing to API connection")
            .accessibilityIdentifier("onboarding.privacy.acknowledged")
        }
        .accessibilityIdentifier("onboarding.page.welcome_privacy")
    }

    private func actionPage(
        eyebrow: String,
        title: String,
        explanation: String,
        check: DayWeaveOnboardingCheck,
        buttonTitle: String,
        buttonSymbol: String,
        action: @escaping () -> Void,
        note: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 24) {
            VStack(alignment: .leading, spacing: 8) {
                Text(eyebrow)
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.tint)
                Text(title)
                    .font(.largeTitle.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text(explanation)
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            OnboardingReadinessCard(check: check)
                .privacySensitive()

            Button(action: action) {
                Label(buttonTitle, systemImage: buttonSymbol)
                    .frame(minWidth: 210)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .accessibilityIdentifier(
                "onboarding.action.\(controller.currentStep.rawValue)"
            )

            Label(note, systemImage: "info.circle")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(15)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 12))
        }
        .accessibilityIdentifier("onboarding.page.\(controller.currentStep.rawValue)")
    }

    private var completionPage: some View {
        VStack(alignment: .leading, spacing: 24) {
            VStack(alignment: .leading, spacing: 9) {
                Text("Your workspace is ready")
                    .font(.largeTitle.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text("DayWeave now has enough context to be useful. Every setting remains editable, and your first plan can adapt as reality changes.")
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            VStack(spacing: 0) {
                completionRow(
                    title: "Privacy boundaries",
                    symbol: "lock.shield",
                    isReady: controller.progress.privacyAcknowledged,
                    detail: "Reviewed"
                )
                Divider().padding(.leading, 46)
                ForEach(completionChecks) { item in
                    completionRow(
                        title: item.step.title,
                        symbol: item.step.symbol,
                        isReady: item.check.isReady,
                        detail: item.check.detail
                    )
                    if item.step != .firstPlan {
                        Divider().padding(.leading, 46)
                    }
                }
            }
            .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 14))
            .overlay {
                RoundedRectangle(cornerRadius: 14).stroke(.quaternary, lineWidth: 1)
            }

            Label(
                "Finish saves only the completion milestone. Account credentials, resource names, item content, and schedule data stay in their existing protected stores.",
                systemImage: "checkmark.shield"
            )
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityIdentifier("onboarding.page.completion")
    }

    private var footer: some View {
        VStack(spacing: 0) {
            if let message = controller.persistenceMessage {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Label(message, systemImage: "externaldrive.badge.exclamationmark")
                        .font(.caption)
                        .foregroundStyle(.red)
                    Spacer(minLength: 8)
                    if controller.persistenceRecoveryRequired {
                        Button("Reset setup progress…", role: .destructive) {
                            showsProgressResetConfirmation = true
                        }
                        .controlSize(.small)
                        .accessibilityIdentifier("onboarding.persistence-reset")
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 26)
                .padding(.top, 10)
                .accessibilityIdentifier("onboarding.persistence-error")
            }

            HStack(spacing: 12) {
                Button("Back") { controller.goBack() }
                    .disabled(!controller.canGoBack)
                    .accessibilityIdentifier("onboarding.back")

                Button("Set up later", action: actions.dismiss)
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
                    .keyboardShortcut(.cancelAction)
                    .accessibilityHint(
                        "Closes setup without marking onboarding complete; the current step is retained"
                    )
                    .accessibilityIdentifier("onboarding.dismiss")

                if let blockingReason = controller.blockingReason(using: readiness) {
                    Label(blockingReason, systemImage: "circle.dashed")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .accessibilityIdentifier("onboarding.blocking-reason")
                }

                Spacer(minLength: 12)

                Button(controller.currentStep == .completion ? "Finish setup" : "Continue") {
                    if controller.currentStep == .completion {
                        if controller.finish(using: readiness) {
                            actions.didComplete()
                        }
                    } else {
                        _ = controller.advance(using: readiness)
                    }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
                .disabled(!controller.canAdvance(using: readiness))
                .accessibilityHint(continueAccessibilityHint)
                .accessibilityIdentifier(
                    controller.currentStep == .completion
                        ? "onboarding.finish" : "onboarding.continue"
                )
            }
            .padding(.horizontal, 26)
            .padding(.vertical, 16)
        }
        .background(.bar)
    }

    private var stepSubtitle: String {
        switch controller.currentStep {
        case .welcomePrivacy: "Understand what stays private before connecting services."
        case .apiConnection: "Use the existing secure device-enrollment workflow."
        case .googleResources: "Select only the sources that belong in your plan."
        case .scheduleProfile: "Set the boundaries the deterministic scheduler must respect."
        case .notifications: "Make reminder permission an explicit choice."
        case .firstItem: "Capture a real commitment without requiring every detail."
        case .firstPlan: "Publish one exact deterministic plan, inspect it, then continue."
        case .completion: "Review live readiness and enter your workspace."
        }
    }

    private var continueAccessibilityHint: String {
        if let reason = controller.blockingReason(using: readiness) {
            return "Unavailable. \(reason)"
        }
        return controller.currentStep == .completion
            ? "Completes onboarding and opens the DayWeave workspace"
            : "Saves progress and opens the next setup step"
    }

    private var completionChecks: [OnboardingCompletionCheck] {
        DayWeaveOnboardingStep.allCases.compactMap { step in
            readiness.check(for: step).map {
                OnboardingCompletionCheck(step: step, check: $0)
            }
        }
    }

    private func onboardingFeature(
        symbol: String,
        title: String,
        detail: String
    ) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: symbol)
                .font(.title3)
                .foregroundStyle(.tint)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(15)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .combine)
    }

    private func completionRow(
        title: String,
        symbol: String,
        isReady: Bool,
        detail: String
    ) -> some View {
        HStack(spacing: 12) {
            Image(systemName: symbol)
                .foregroundStyle(.tint)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.medium))
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 8)
            Image(systemName: isReady ? "checkmark.circle.fill" : "circle.dashed")
                .foregroundStyle(isReady ? Color.green : Color.secondary)
                .accessibilityLabel(isReady ? "Ready" : "Not ready")
        }
        .padding(.horizontal, 15)
        .padding(.vertical, 12)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private func stepMarker(_ step: DayWeaveOnboardingStep) -> some View {
        if controller.currentStep == step {
            Image(systemName: "circle.inset.filled")
                .foregroundStyle(.tint)
        } else if step.ordinal < controller.progress.furthestReachedStep.ordinal {
            if liveReadiness(for: step) == false {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
            } else {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            }
        } else {
            Image(systemName: step.symbol)
                .foregroundStyle(stepForeground(step))
        }
    }

    private func stepForeground(_ step: DayWeaveOnboardingStep) -> Color {
        if controller.currentStep == step { return .primary }
        return controller.canNavigate(to: step)
            ? Color.secondary
            : Color.secondary.opacity(0.45)
    }

    private func stepAccessibilityValue(_ step: DayWeaveOnboardingStep) -> String {
        let liveStatus = liveReadiness(for: step).map { $0 ? "ready" : "needs attention" }
        if controller.currentStep == step {
            return liveStatus.map { "Current step, \($0)" } ?? "Current step"
        }
        if step.ordinal < controller.progress.furthestReachedStep.ordinal {
            return liveStatus.map { "Previously visited, \($0)" } ?? "Previously visited"
        }
        return controller.canNavigate(to: step) ? "Available" : "Not yet available"
    }

    private func liveReadiness(for step: DayWeaveOnboardingStep) -> Bool? {
        switch step {
        case .welcomePrivacy:
            controller.progress.privacyAcknowledged
        case .completion:
            controller.progress.privacyAcknowledged
                && readiness.firstIncompleteStep == nil
        default:
            readiness.check(for: step)?.isReady
        }
    }
}

private struct OnboardingCompletionCheck: Identifiable {
    let step: DayWeaveOnboardingStep
    let check: DayWeaveOnboardingCheck

    var id: DayWeaveOnboardingStep { step }
}

private struct OnboardingReadinessCard: View {
    let check: DayWeaveOnboardingCheck

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            ZStack {
                Circle()
                    .fill(statusColor.opacity(0.14))
                if check.state == .working {
                    ProgressView().controlSize(.small)
                } else {
                    Image(systemName: statusSymbol)
                        .font(.headline)
                        .foregroundStyle(statusColor)
                }
            }
            .frame(width: 38, height: 38)
            .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 4) {
                Text(statusTitle)
                    .font(.headline)
                Text(check.detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(statusColor.opacity(0.07), in: RoundedRectangle(cornerRadius: 13))
        .overlay {
            RoundedRectangle(cornerRadius: 13)
                .stroke(statusColor.opacity(0.2), lineWidth: 1)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(statusTitle). \(check.detail)")
        .accessibilityIdentifier("onboarding.readiness")
    }

    private var statusTitle: String {
        switch check.state {
        case .pending: "Action needed"
        case .working: "Checking readiness"
        case .ready: "Ready to continue"
        case .blocked: "Needs attention"
        }
    }

    private var statusSymbol: String {
        switch check.state {
        case .pending: "circle.dashed"
        case .working: "arrow.triangle.2.circlepath"
        case .ready: "checkmark.circle.fill"
        case .blocked: "exclamationmark.triangle.fill"
        }
    }

    private var statusColor: Color {
        switch check.state {
        case .pending: .secondary
        case .working: .blue
        case .ready: .green
        case .blocked: .orange
        }
    }
}
