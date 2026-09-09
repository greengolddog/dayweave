package com.greengolddog.dayweave.model

internal const val PROGRESS_ITEM = "00000000-0000-4000-8000-000000000001"
internal const val PROGRESS_OPERATION = "00000000-0000-4000-8000-000000000002"
internal const val PROGRESS_COMPONENT = "00000000-0000-4000-8000-000000000003"
internal const val PROGRESS_NOW = "2026-09-08T09:00:00Z"
internal const val PROGRESS_ORIGIN = "https://api.example.test/"
internal const val PROGRESS_CONFIGURATION = "configuration-a"

internal fun progressTestItem(id: String = PROGRESS_ITEM) = CanonicalItemSnapshot(id = id,
    kind = "project", status = "blocked", title = "Synthetic progress item", timezoneName = "UTC",
    flexibleConstraintsJson = "{}", splitPolicyJson = "{\"mode\":\"never\"}", importance = 1,
    urgency = 1, siblingOrder = 0, isExecutable = false, revision = 7,
    createdAt = PROGRESS_NOW, updatedAt = PROGRESS_NOW)
internal fun progressTestComponents() = listOf(ItemProgressComponent(PROGRESS_COMPONENT, "Synthetic measure", ItemProgressValue.Percentage(4250)))
internal fun progressTestSnapshot(revision: Long = 0, itemRevision: Long = 7) = ItemProgressSnapshot(1, PROGRESS_ITEM,
    itemRevision, revision, if (revision == 0L) emptyList() else progressTestComponents(),
    if (revision == 0L) null else PROGRESS_NOW)
internal fun progressTestLedger(snapshot: ItemProgressSnapshot = progressTestSnapshot()) = ItemProgressLedger(
    syncOrigin = PROGRESS_ORIGIN, configurationId = PROGRESS_CONFIGURATION,
    observations = mapOf(snapshot.itemId to ItemProgressObservation(snapshot, PROGRESS_NOW, true)))
internal fun progressTestState() = DayWeaveUiState(canonicalItems = listOf(progressTestItem()),
    canonicalSyncOrigin = PROGRESS_ORIGIN, canonicalConfigurationId = PROGRESS_CONFIGURATION,
    canonicalDeltaCursor = "synthetic-complete-cursor", itemProgressLedger = progressTestLedger())
internal fun progressTestMutation(submitted: Boolean = false, sensitive: Boolean = false) = PendingItemProgressMutation(
    operationId = PROGRESS_OPERATION, itemId = PROGRESS_ITEM, syncOrigin = PROGRESS_ORIGIN,
    configurationId = PROGRESS_CONFIGURATION, expectedItemRevision = 7, expectedProgressRevision = 0,
    requestJson = ITEM_PROGRESS_JSON.encodeToString(ItemProgressRequest(operationId = PROGRESS_OPERATION,
        expectedItemRevision = 7, expectedProgressRevision = 0, components = progressTestComponents())),
    createdAt = PROGRESS_NOW, submittedAt = PROGRESS_NOW.takeIf { submitted }, wasSensitive = sensitive)
