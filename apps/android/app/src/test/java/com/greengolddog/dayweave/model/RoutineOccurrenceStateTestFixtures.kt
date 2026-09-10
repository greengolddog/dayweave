package com.greengolddog.dayweave.model

internal fun routineStateTestLedger(snapshot: RoutineOccurrenceSnapshot = routineTestSnapshot()) = RoutineOccurrenceLedger(
    syncOrigin = PROGRESS_ORIGIN, configurationId = PROGRESS_CONFIGURATION,
    observations = mapOf(snapshot.aggregate.manifest.id to RoutineOccurrenceObservation(snapshot, ROUTINE_NOW)))

internal fun routineStateTestIntent(submitted: Boolean = false, snapshot: RoutineOccurrenceSnapshot = routineTestSnapshot(),
    operation: String = ROUTINE_OPERATION): PendingRoutineOccurrenceMutation {
    val request = routineTestRequest().copy(operationId = operation,
        expectedInstanceRevision = snapshot.aggregate.revision,
        expectedMemberRevision = snapshot.aggregate.members.single { it.itemId == ROUTINE_CHILD }.revision,
        expectedEvidenceHash = snapshot.evidenceHash)
    return PendingRoutineOccurrenceMutation(operationId = operation, instanceId = snapshot.aggregate.manifest.id,
        memberId = ROUTINE_CHILD, syncOrigin = PROGRESS_ORIGIN, configurationId = PROGRESS_CONFIGURATION,
        requestJson = " \n" + ITEM_PROGRESS_JSON.encodeToString(request) + "\n ", request = request,
        createdAt = ROUTINE_NOW, submittedAt = ROUTINE_NOW.takeIf { submitted })
}

internal fun routineStateTestUi(ledger: RoutineOccurrenceLedger = routineStateTestLedger()) = DayWeaveUiState(
    canonicalSyncOrigin = PROGRESS_ORIGIN, canonicalConfigurationId = PROGRESS_CONFIGURATION,
    routineOccurrenceLedger = ledger)

internal fun routineStateTestId(number: Int) = "00000000-0000-0000-0000-${number.toString().padStart(12, '0')}"

internal fun routineStateTestInstance(number: Int, members: Int = 3, title: String = "Synthetic optional leaf"): RoutineOccurrenceSnapshot {
    val original = routineTestSnapshot()
    val manifest = original.aggregate.manifest.copy(id = routineStateTestId(number),
        occurrenceId = "10000000-0000-5000-8000-${number.toString().padStart(12, '0')}")
    val additional = (3 until members).map { index -> manifest.members.last().copy(
        itemId = routineStateTestId(10_000 + index), siblingOrder = index, title = title) }
    return original.copy(aggregate = original.aggregate.copy(manifest = manifest.copy(members = manifest.members + additional),
        members = original.aggregate.members + additional.map { original.aggregate.members.last().copy(itemId = it.itemId) }),
        members = original.members + additional.map { original.members.last().copy(itemId = it.itemId) })
}
