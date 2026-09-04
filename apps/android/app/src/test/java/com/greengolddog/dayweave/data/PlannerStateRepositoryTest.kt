package com.greengolddog.dayweave.data

import com.greengolddog.dayweave.model.CanonicalExecutionSessionSnapshot
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalPlanUpdate
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.ExecutionDeferAssessmentSnapshot
import com.greengolddog.dayweave.model.ExecutionDeferViolationSnapshot
import com.greengolddog.dayweave.model.GoogleCalendarOutboundJournal
import com.greengolddog.dayweave.model.GoogleCalendarOutboundPreviewSnapshot
import com.greengolddog.dayweave.model.GoogleCalendarOutboundStage
import com.greengolddog.dayweave.model.ItemKind
import com.greengolddog.dayweave.model.ItemStatus
import com.greengolddog.dayweave.model.LocalScheduleCompositionProvenanceSnapshot
import com.greengolddog.dayweave.model.InboxItem
import com.greengolddog.dayweave.model.InboxSource
import com.greengolddog.dayweave.model.ScheduleItem
import com.greengolddog.dayweave.model.ScheduleCompositionProfileSnapshot
import com.greengolddog.dayweave.model.PendingSchedulePublication
import com.greengolddog.dayweave.model.PendingExecutionDeferIntent
import com.greengolddog.dayweave.model.PendingProposalApplicationMutation
import com.greengolddog.dayweave.model.ProposalApplicationMutationKind
import com.greengolddog.dayweave.model.ProposalApplicationReceiptSnapshot
import com.greengolddog.dayweave.model.ProposalApplicationStatusSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleBlockProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleProofSnapshot
import com.greengolddog.dayweave.model.PublishedScheduleRevisionSnapshot
import com.greengolddog.dayweave.model.RecurrenceMoveSnapshot
import com.greengolddog.dayweave.model.RecurrenceOccurrenceSourceSnapshot
import com.greengolddog.dayweave.model.TerminalExecutionOutcomeSnapshot
import com.greengolddog.dayweave.model.localScheduleCompositionStateFingerprint
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.GoogleCalendarOutboundEntityKind
import com.greengolddog.dayweave.network.GoogleCalendarOutboundOperation
import com.greengolddog.dayweave.network.RemoteGoogleOutboundApproval
import com.greengolddog.dayweave.network.ScheduleAvailabilityRequest
import com.greengolddog.dayweave.network.SchedulePreviewRequest
import com.greengolddog.dayweave.network.SchedulePublishRequest
import com.greengolddog.dayweave.network.buildSchedulePublishHttpRequest
import com.greengolddog.dayweave.network.prepareProposalApplyHttpRequest
import com.greengolddog.dayweave.network.prepareProposalUndoHttpRequest
import com.greengolddog.dayweave.state.PlannerLoadState
import com.greengolddog.dayweave.state.PlannerStore
import java.time.Instant
import java.time.ZoneId
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test

class PlannerStateRepositoryTest {
    @Test
    fun v15CanonicalItemsGainTypedLegacyInferenceWithoutTrustingInjectedStructure() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 1_000 }
        repository.save(DayWeaveUiState(canonicalItems = listOf(sensitiveCanonicalItem())))
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        val item = (root.getValue("canonicalItems") as JsonArray).single().jsonObject
        val injected = JsonObject(
            item + mapOf(
                "durationKind" to JsonPrimitive("range"),
                "durationMinSeconds" to JsonPrimitive(300),
                "durationMaxSeconds" to JsonPrimitive(7_200),
                "durationSource" to JsonPrimitive("assistant"),
                "hasExplicitStructuralMetadata" to JsonPrimitive(true),
            ),
        )
        dao.snapshot = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root + ("canonicalItems" to JsonArray(listOf(injected)))),
            ),
            payloadFormat = PlannerSnapshotFormats.JSON_V15,
        )

        val restored = requireNotNull(repository.load())
        val restoredItem = restored.canonicalItems.single()

        assertEquals(CanonicalDurationKind.EXACT, restoredItem.durationKind)
        assertEquals(1_800L, restoredItem.durationMinSeconds)
        assertEquals(1_800L, restoredItem.durationMaxSeconds)
        assertEquals(CanonicalDurationSource.USER, restoredItem.durationSource)
        assertFalse(restoredItem.hasExplicitStructuralMetadata)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        val rewritten = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload).jsonObject
        val rewrittenItem = (rewritten.getValue("canonicalItems") as JsonArray)
            .single().jsonObject
        assertTrue(rewrittenItem.keys.containsAll(CANONICAL_STRUCTURAL_TEST_FIELDS))
    }

    @Test
    fun v16RequiresCompleteStructureAndRejectsRichMetadataWithLegacyMarker() = runBlocking {
        listOf<(JsonObject) -> JsonObject>(
            { item -> JsonObject(item - "blockedReason") },
            { item ->
                JsonObject(
                    item + mapOf(
                        "durationKind" to JsonPrimitive("range"),
                        "durationMinSeconds" to JsonPrimitive(300),
                        "durationMaxSeconds" to JsonPrimitive(7_200),
                        "hasExplicitStructuralMetadata" to JsonPrimitive(false),
                    ),
                )
            },
        ).forEach { mutate ->
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { 1_000 }
            repository.save(DayWeaveUiState(canonicalItems = listOf(sensitiveCanonicalItem())))
            val current = requireNotNull(dao.snapshot)
            val root = Json.parseToJsonElement(current.payload).jsonObject
            val item = (root.getValue("canonicalItems") as JsonArray).single().jsonObject
            val malformed = current.copy(
                payload = Json.encodeToString(
                    JsonObject.serializer(),
                    JsonObject(
                        root + ("canonicalItems" to JsonArray(listOf(mutate(item)))),
                    ),
                ),
            )
            dao.snapshot = malformed

            assertThrows(SerializationException::class.java) {
                runBlocking { repository.load() }
            }
            assertEquals(malformed, dao.snapshot)
        }
    }

    @Test
    fun v1ThroughV9LabelsDiscardInjectedNewerPolicyAndOutboundAuthority() = runBlocking {
        val predecessorFormats = listOf(
            PlannerSnapshotFormats.JSON_V1,
            PlannerSnapshotFormats.JSON_V2,
            PlannerSnapshotFormats.JSON_V3,
            PlannerSnapshotFormats.JSON_V4,
            PlannerSnapshotFormats.JSON_V5,
            PlannerSnapshotFormats.JSON_V6,
            PlannerSnapshotFormats.JSON_V7,
            PlannerSnapshotFormats.JSON_V8,
            PlannerSnapshotFormats.JSON_V9,
        )
        predecessorFormats.forEach { predecessor ->
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { 2 }
            repository.save(DayWeaveUiState())
            val current = requireNotNull(dao.snapshot)
            val root = Json.parseToJsonElement(current.payload).jsonObject
            val injected = JsonObject(
                root + mapOf(
                    "localScheduleCompositionProvenance" to JsonObject(
                        mapOf("schemaVersion" to JsonPrimitive(999)),
                    ),
                    "scheduleCompositionProfile" to JsonObject(
                        mapOf(
                            "dayStartMinute" to JsonPrimitive(600),
                            "dayEndMinute" to JsonPrimitive(601),
                            "slotGranularityMinutes" to JsonPrimitive(60),
                            "stabilityWeight" to JsonPrimitive(999),
                            "defaultSoftWeight" to JsonPrimitive(999),
                        ),
                    ),
                    "pendingGoogleCalendarOutbound" to Json.encodeToJsonElement(
                        GoogleCalendarOutboundJournal.serializer(),
                        validOutboundJournal(),
                    ),
                ),
            )
            dao.snapshot = current.copy(
                payload = Json.encodeToString(JsonObject.serializer(), injected),
                payloadFormat = predecessor,
            )

            val restored = requireNotNull(repository.load())

            assertEquals(null, restored.localScheduleCompositionProvenance)
            assertEquals(ScheduleCompositionProfileSnapshot(), restored.scheduleCompositionProfile)
            assertEquals(null, restored.pendingGoogleCalendarOutbound)
            assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        }
    }

    @Test
    fun v13RequiresLocalProvenanceAndSchedulingProfileRootFields() = runBlocking {
        listOf(
            "localScheduleCompositionProvenance",
            "scheduleCompositionProfile",
            "pendingGoogleCalendarOutbound",
        ).forEach { missingField ->
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { 4 }
            repository.save(DayWeaveUiState())
            val current = requireNotNull(dao.snapshot)
            val root = Json.parseToJsonElement(current.payload).jsonObject
            val missing = current.copy(
                payload = Json.encodeToString(
                    JsonObject.serializer(),
                    JsonObject(root - missingField),
                ),
            )
            dao.snapshot = missing

            assertThrows(SerializationException::class.java) {
                runBlocking { repository.load() }
            }
            assertEquals(missing, dao.snapshot)
        }
    }

    @Test
    fun v13RequiresExplicitNestedFirmHorizonDays() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 5 }
        repository.save(DayWeaveUiState())
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        val profile = root.getValue("scheduleCompositionProfile").jsonObject
        assertEquals(JsonPrimitive(7), profile["firmHorizonDays"])
        val missingHorizon = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root +
                        ("scheduleCompositionProfile" to
                            JsonObject(profile - "firmHorizonDays")),
                ),
            ),
        )
        dao.snapshot = missingHorizon

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(missingHorizon, dao.snapshot)
    }

    @Test
    fun v10AndV11DefaultOrIgnoreFirmHorizonAndPreserveExactPublicationJournal() = runBlocking {
        listOf(PlannerSnapshotFormats.JSON_V10, PlannerSnapshotFormats.JSON_V11)
            .forEach { predecessor ->
                listOf<Int?>(null, 23).forEach { injectedDays ->
                    val dao = FakePlannerSnapshotDao()
                    val repository = RoomPlannerStateRepository(dao) { 7 }
                    val original = pendingPublicationState().copy(
                        scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(
                            dayStartMinute = 10 * 60,
                        ),
                    )
                    repository.save(original)
                    val current = requireNotNull(dao.snapshot)
                    val root = Json.parseToJsonElement(current.payload).jsonObject
                    val exactPending = root.getValue("pendingSchedulePublication")
                    val profile = root.getValue("scheduleCompositionProfile").jsonObject
                    val predecessorProfile = JsonObject(
                        (profile - "firmHorizonDays") +
                            listOfNotNull(
                                injectedDays?.let {
                                    "firmHorizonDays" to JsonPrimitive(it)
                                },
                            ),
                    )
                    dao.snapshot = current.copy(
                        payload = Json.encodeToString(
                            JsonObject.serializer(),
                            JsonObject(
                                root +
                                    ("scheduleCompositionProfile" to predecessorProfile),
                            ),
                        ),
                        payloadFormat = predecessor,
                    )

                    val restored = requireNotNull(repository.load())
                    val rewritten = requireNotNull(dao.snapshot)
                    val rewrittenRoot = Json.parseToJsonElement(rewritten.payload).jsonObject

                    assertEquals(10 * 60, restored.scheduleCompositionProfile.dayStartMinute)
                    assertEquals(7, restored.scheduleCompositionProfile.firmHorizonDays)
                    assertEquals(
                        original.pendingSchedulePublication,
                        restored.pendingSchedulePublication,
                    )
                    assertEquals(exactPending, rewrittenRoot["pendingSchedulePublication"])
                    assertEquals(
                        original.pendingSchedulePublication?.request?.bodyJson,
                        restored.pendingSchedulePublication?.request?.bodyJson,
                    )
                    assertEquals(PlannerSnapshotFormats.JSON_V16, rewritten.payloadFormat)
                    assertEquals(
                        JsonPrimitive(7),
                        rewrittenRoot.getValue("scheduleCompositionProfile")
                            .jsonObject["firmHorizonDays"],
                    )
                }
        }
    }

    @Test
    fun v11PreservesPublishedHavanaProofUsingTheLegacyEarlierMidnightOffset() = runBlocking {
        val origin = "https://api.example.test/"
        val configurationId = "connection-1"
        val start = "2026-11-01T04:00:00Z"
        val end = "2026-11-08T05:00:00Z"
        val digest = "sha256:${"c".repeat(64)}"
        val revisionId = "33333333-3333-4333-8333-333333333333"
        val revision = PublishedScheduleRevisionSnapshot(
            id = revisionId,
            revision = "4:$revisionId",
            revisionNumber = 4uL,
            inputDigest = digest,
            horizonStart = start,
            horizonEnd = end,
            timezoneName = "America/Havana",
            publishedAt = start,
        )
        val proof = PublishedScheduleProofSnapshot(
            schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
            syncOrigin = origin,
            configurationId = configurationId,
            revision = revision,
            asOf = start,
            blocks = emptyList(),
        )
        val original = DayWeaveUiState(
            canonicalSyncOrigin = origin,
            canonicalConfigurationId = configurationId,
            canonicalDeltaCursor = "cursor-havana",
            publishedScheduleRevision = revision,
            publishedScheduleProof = proof,
            scheduleInputDigest = digest,
            scheduleGeneratedAt = start,
            schedulePlanningZoneId = revision.timezoneName,
        )
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 11 }
        repository.save(original)
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        val profile = root.getValue("scheduleCompositionProfile").jsonObject
        dao.snapshot = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root +
                        ("scheduleCompositionProfile" to
                            JsonObject(profile - "firmHorizonDays")),
                ),
            ),
            payloadFormat = PlannerSnapshotFormats.JSON_V11,
        )

        val restored = requireNotNull(repository.load())

        assertEquals(proof, restored.publishedScheduleProof)
        assertEquals(revision, restored.publishedScheduleRevision)
        assertTrue(
            restored.isPublishedScheduleDisplayCurrent(
                Instant.parse("2026-11-01T04:30:00Z"),
                ZoneId.of("America/Havana"),
            ),
        )
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
    }

    @Test
    fun v11OneDayLocalProvenanceLosesDisplayAuthorityUnderDefaultProfile() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 8 }
        repository.save(localProvenanceState(horizonDays = 1, profileDays = 1))
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        val profile = root.getValue("scheduleCompositionProfile").jsonObject
        dao.snapshot = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root +
                        ("scheduleCompositionProfile" to
                            JsonObject(profile - "firmHorizonDays")),
                ),
            ),
            payloadFormat = PlannerSnapshotFormats.JSON_V11,
        )

        val restored = requireNotNull(repository.load())

        assertEquals(7, restored.scheduleCompositionProfile.firmHorizonDays)
        assertEquals(null, restored.localScheduleCompositionProvenance)
        assertFalse(
            restored.isScheduleDisplayCurrent(
                Instant.parse("2026-08-29T12:00:00Z"),
                ZoneId.of("UTC"),
            ),
        )
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
    }

    @Test
    fun v10AndV11CannotRetainInjectedMultiDayLocalDisplayAuthority() = runBlocking {
        listOf(PlannerSnapshotFormats.JSON_V10, PlannerSnapshotFormats.JSON_V11)
            .forEach { predecessor ->
                val dao = FakePlannerSnapshotDao()
                val repository = RoomPlannerStateRepository(dao) { 9 }
                repository.save(localProvenanceState(horizonDays = 7, profileDays = 7))
                val injected = requireNotNull(dao.snapshot).copy(payloadFormat = predecessor)
                dao.snapshot = injected

                val restored = requireNotNull(repository.load())

                assertEquals(7, restored.scheduleCompositionProfile.firmHorizonDays)
                assertEquals(null, restored.localScheduleCompositionProvenance)
                assertFalse(
                    restored.isScheduleDisplayCurrent(
                        Instant.parse("2026-08-29T12:00:00Z"),
                        ZoneId.of("UTC"),
                    ),
                )
                assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
            }
    }

    @Test
    fun v10CannotSmuggleOutboundAuthorityAndMigratesWithNullAuthority() = runBlocking {
        listOf(true, false).forEach { injectNewerField ->
            val now = Instant.parse("2026-09-02T12:10:00Z").toEpochMilli()
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { now }
            repository.save(outboundState(validOutboundJournal()))
            val current = requireNotNull(dao.snapshot)
            val root = Json.parseToJsonElement(current.payload).jsonObject
            val v10Payload = if (injectNewerField) root else {
                JsonObject(root - "pendingGoogleCalendarOutbound")
            }
            dao.snapshot = current.copy(
                payload = Json.encodeToString(JsonObject.serializer(), v10Payload),
                payloadFormat = PlannerSnapshotFormats.JSON_V10,
            )

            val restored = requireNotNull(repository.load())

            assertEquals(null, restored.pendingGoogleCalendarOutbound)
            assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
            assertTrue(
                requireNotNull(dao.snapshot).payload.contains(
                    "\"pendingGoogleCalendarOutbound\":null",
                ),
            )
            assertFalse(requireNotNull(dao.snapshot).payload.contains(OUTBOUND_CAPABILITY))
        }
    }

    @Test
    fun v11AndV12LegacyCalendarJournalsCannotAcquireTaskDeleteAuthority() = runBlocking {
        listOf(PlannerSnapshotFormats.JSON_V11, PlannerSnapshotFormats.JSON_V12)
            .forEach { predecessor ->
                listOf(false, true).forEach { injectTaskDelete ->
                    val now = Instant.parse("2026-09-02T12:10:00Z").toEpochMilli()
                    val dao = FakePlannerSnapshotDao()
                    val repository = RoomPlannerStateRepository(dao) { now }
                    val expected = validOutboundJournal()
                    repository.save(outboundState(expected))
                    val current = requireNotNull(dao.snapshot)
                    var root = Json.parseToJsonElement(current.payload).jsonObject
                    val currentJournal = root.getValue("pendingGoogleCalendarOutbound").jsonObject
                    val legacyFields = (currentJournal - "entityKind") + mapOf(
                        "schemaVersion" to JsonPrimitive(1),
                        "operation" to JsonPrimitive(
                            if (injectTaskDelete) "delete" else "upsert",
                        ),
                    )
                    val legacyJournal = if (injectTaskDelete) {
                        JsonObject(legacyFields + ("entityKind" to JsonPrimitive("task")))
                    } else {
                        JsonObject(legacyFields)
                    }
                    root = JsonObject(root + ("pendingGoogleCalendarOutbound" to legacyJournal))
                    if (predecessor == PlannerSnapshotFormats.JSON_V11) {
                        val profile = root.getValue("scheduleCompositionProfile").jsonObject
                        root = JsonObject(
                            root + ("scheduleCompositionProfile" to
                                JsonObject(profile - "firmHorizonDays")),
                        )
                    }
                    dao.snapshot = current.copy(
                        payload = Json.encodeToString(JsonObject.serializer(), root),
                        payloadFormat = predecessor,
                    )

                    val restored = requireNotNull(repository.load())
                    val restoredJournal = requireNotNull(restored.pendingGoogleCalendarOutbound)
                    val rewritten = requireNotNull(dao.snapshot)
                    val rewrittenJournal = Json.parseToJsonElement(rewritten.payload)
                        .jsonObject
                        .getValue("pendingGoogleCalendarOutbound")
                        .jsonObject

                    assertEquals(expected, restoredJournal)
                    assertEquals(2, restoredJournal.schemaVersion)
                    assertEquals(
                        GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
                        restoredJournal.entityKind,
                    )
                    assertEquals(
                        GoogleCalendarOutboundOperation.UPSERT,
                        restoredJournal.operation,
                    )
                    assertEquals(PlannerSnapshotFormats.JSON_V16, rewritten.payloadFormat)
                    assertEquals(JsonPrimitive(2), rewrittenJournal["schemaVersion"])
                    assertEquals(
                        JsonPrimitive("calendar_event"),
                        rewrittenJournal["entityKind"],
                    )
                    assertEquals(JsonPrimitive("upsert"), rewrittenJournal["operation"])
                }
            }
    }

    @Test
    fun v11AndV12InjectedTaskDeletePreviewFailsClosedInsteadOfBeingNormalized() = runBlocking {
        listOf(PlannerSnapshotFormats.JSON_V11, PlannerSnapshotFormats.JSON_V12)
            .forEach { predecessor ->
                val now = Instant.parse("2026-09-02T12:10:00Z").toEpochMilli()
                val dao = FakePlannerSnapshotDao()
                val repository = RoomPlannerStateRepository(dao) { now }
                repository.save(outboundState(validOutboundJournal()))
                val current = requireNotNull(dao.snapshot)
                var root = Json.parseToJsonElement(current.payload).jsonObject
                val journal = root.getValue("pendingGoogleCalendarOutbound").jsonObject
                val preview = journal.getValue("preview").jsonObject
                val injectedPreview = JsonObject(
                    preview + mapOf(
                        "entityKind" to JsonPrimitive("task"),
                        "operation" to JsonPrimitive("delete"),
                        "providerResourceId" to JsonPrimitive("provider-resource-1"),
                        "providerEtag" to JsonPrimitive("provider-etag-1"),
                        "providerPayload" to JsonObject(emptyMap()),
                    ),
                )
                val injectedJournal = JsonObject(
                    journal + mapOf(
                        "schemaVersion" to JsonPrimitive(1),
                        "entityKind" to JsonPrimitive("task"),
                        "operation" to JsonPrimitive("delete"),
                        "preview" to injectedPreview,
                    ),
                )
                root = JsonObject(root + ("pendingGoogleCalendarOutbound" to injectedJournal))
                if (predecessor == PlannerSnapshotFormats.JSON_V11) {
                    val profile = root.getValue("scheduleCompositionProfile").jsonObject
                    root = JsonObject(
                        root + ("scheduleCompositionProfile" to
                            JsonObject(profile - "firmHorizonDays")),
                    )
                }
                val injected = current.copy(
                    payload = Json.encodeToString(JsonObject.serializer(), root),
                    payloadFormat = predecessor,
                )
                dao.snapshot = injected

                assertThrows(IllegalArgumentException::class.java) {
                    runBlocking { repository.load() }
                }
                assertEquals(injected, dao.snapshot)
            }
    }

    @Test
    fun v13JournalRequiresExplicitGeneralizedOutboundIdentity() = runBlocking {
        val now = Instant.parse("2026-09-02T12:10:00Z").toEpochMilli()
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { now }
        repository.save(outboundState(validOutboundJournal()))
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload).jsonObject
        val journal = root.getValue("pendingGoogleCalendarOutbound").jsonObject
        val missingIdentity = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root + ("pendingGoogleCalendarOutbound" to
                        JsonObject(journal - "entityKind")),
                ),
            ),
        )
        dao.snapshot = missingIdentity

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(missingIdentity, dao.snapshot)
    }

    @Test
    fun currentAndExpiredOutboundRecoveryJournalsRoundTripExactly() = runBlocking {
        val now = Instant.parse("2026-09-02T12:10:00Z")
        val expiredAttempt = validOutboundJournal(
            stage = GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED,
            createdAt = "2026-09-02T10:00:00Z",
            intentExpiresAt = "2026-09-02T10:30:00Z",
            previewExpiresAt = "2026-09-02T10:20:00Z",
            approvalExpiresAt = "2026-09-02T10:15:00Z",
        )
        val expiredApproved = validOutboundJournal(
            stage = GoogleCalendarOutboundStage.APPROVED,
            createdAt = "2026-09-02T10:00:00Z",
            intentExpiresAt = "2026-09-02T10:30:00Z",
            previewExpiresAt = "2026-09-02T10:20:00Z",
            approvalExpiresAt = "2026-09-02T10:15:00Z",
        )
        assertTrue(expiredAttempt.canDiscardExpiredAt(now))
        assertTrue(expiredApproved.canDiscardExpiredAt(now))

        (
            GoogleCalendarOutboundStage.entries.map { validOutboundJournal(stage = it) } +
                listOf(expiredAttempt, expiredApproved)
        ).forEach { journal ->
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { now.toEpochMilli() }

            repository.save(outboundState(journal))
            val restored = requireNotNull(repository.load())

            assertEquals(journal, restored.pendingGoogleCalendarOutbound)
            assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        }
    }

    @Test
    fun generalizedTaskAndDeleteOutboundJournalsRoundTripExactly() = runBlocking {
        val now = Instant.parse("2026-09-02T12:10:00Z")
        listOf(
            GoogleCalendarOutboundEntityKind.TASK to GoogleCalendarOutboundOperation.UPSERT,
            GoogleCalendarOutboundEntityKind.TASK to GoogleCalendarOutboundOperation.DELETE,
            GoogleCalendarOutboundEntityKind.CALENDAR_EVENT to
                GoogleCalendarOutboundOperation.DELETE,
        ).forEach { (entityKind, operation) ->
            val journal = validOutboundJournal(
                entityKind = entityKind,
                operation = operation,
            )
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { now.toEpochMilli() }

            repository.save(outboundState(journal))
            val restored = requireNotNull(repository.load())

            assertEquals(journal, restored.pendingGoogleCalendarOutbound)
            assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        }
    }

    @Test
    fun malformedAndFutureCreatedOutboundJournalsFailClosed() = runBlocking {
        val now = Instant.parse("2026-09-02T12:00:00Z").toEpochMilli()
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { now }
        repository.save(outboundState(validOutboundJournal()))
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val journal = root.getValue("pendingGoogleCalendarOutbound").jsonObject
        dao.snapshot = stored.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(
                    root + (
                        "pendingGoogleCalendarOutbound" to JsonObject(
                            journal + ("operationGeneration" to JsonPrimitive(0)),
                        )
                    ),
                ),
            ),
        )
        val malformed = requireNotNull(dao.snapshot)

        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(malformed, dao.snapshot)

        val futureCreated = validOutboundJournal(
            stage = GoogleCalendarOutboundStage.INTENT,
            createdAt = "2026-09-02T12:05:01Z",
            intentExpiresAt = "2026-09-02T12:30:00Z",
            previewExpiresAt = "2026-09-02T12:20:00Z",
            approvalExpiresAt = "2026-09-02T12:15:00Z",
        )
        val directDao = FakePlannerSnapshotDao()
        assertThrows(SerializationException::class.java) {
            runBlocking {
                RoomPlannerStateRepository(directDao) { now }.save(
                    outboundState(futureCreated),
                )
            }
        }
        assertEquals(null, directDao.snapshot)
    }

    @Test
    fun outboundJournalCannotCrossCanonicalApiBindingOnSaveOrLoad() = runBlocking {
        val now = Instant.parse("2026-09-02T12:10:00Z").toEpochMilli()
        val journal = validOutboundJournal()
        val directDao = FakePlannerSnapshotDao()
        assertThrows(SerializationException::class.java) {
            runBlocking {
                RoomPlannerStateRepository(directDao) { now }.save(
                    outboundState(journal).copy(canonicalConfigurationId = "other-binding"),
                )
            }
        }
        assertEquals(null, directDao.snapshot)

        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { now }
        repository.save(outboundState(journal))
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        dao.snapshot = stored.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root + ("canonicalSyncOrigin" to JsonPrimitive("https://other.test/"))),
            ),
        )
        val mismatched = requireNotNull(dao.snapshot)

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(mismatched, dao.snapshot)
    }

    @Test
    fun malformedSchedulingProfileFailsCurrentLoadAndDirectSave() = runBlocking {
        val invalid = ScheduleCompositionProfileSnapshot(
            dayStartMinute = 1_000,
            dayEndMinute = 900,
        )
        val directDao = FakePlannerSnapshotDao()
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking {
                RoomPlannerStateRepository(directDao).save(
                    DayWeaveUiState(scheduleCompositionProfile = invalid),
                )
            }
        }
        assertEquals(null, directDao.snapshot)

        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao)
        repository.save(DayWeaveUiState())
        val stored = requireNotNull(dao.snapshot)
        dao.snapshot = stored.copy(
            payload = stored.payload.replace(
                "\"dayStartMinute\":420,\"dayEndMinute\":1320",
                "\"dayStartMinute\":1000,\"dayEndMinute\":900",
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun staleReorderedV13ProvenanceIsQuarantinedBeforeOneCanonicalRewrite() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 6 }
        val valid = localProvenanceState()
        repository.save(valid)
        val stored = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(stored.payload).jsonObject
        val provenance = root.getValue("localScheduleCompositionProvenance").jsonObject
        val mismatched = JsonObject(provenance + ("deltaCursor" to JsonPrimitive("other-cursor")))
        val reordered = linkedMapOf<String, kotlinx.serialization.json.JsonElement>()
        root.entries.reversed().forEach { (key, value) ->
            reordered[key] = if (key == "localScheduleCompositionProvenance") mismatched else value
        }
        dao.snapshot = stored.copy(
            payload = Json.encodeToString(JsonObject.serializer(), JsonObject(reordered)),
        )
        val beforeLoadSaves = dao.saveCount

        val restored = requireNotNull(repository.load())

        assertEquals(null, restored.localScheduleCompositionProvenance)
        assertEquals(beforeLoadSaves + 1, dao.saveCount)
        assertFalse(requireNotNull(dao.snapshot).payload.contains("other-cursor"))
    }

    @Test
    fun mismatchedLocalProvenanceCannotBeSavedDirectly() {
        val valid = localProvenanceState()
        val mismatched = valid.copy(canonicalDeltaCursor = "other-cursor")
        val dao = FakePlannerSnapshotDao()

        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { RoomPlannerStateRepository(dao).save(mismatched) }
        }
        assertEquals(null, dao.snapshot)
    }

    @Test
    fun v8SnapshotWithoutNotificationReceiptsUpgradesToV13WithNoSuppressionAuthority() =
        runBlocking {
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { 4 }
            repository.save(DayWeaveUiState())
            val current = requireNotNull(dao.snapshot)
            val root = Json.parseToJsonElement(current.payload) as JsonObject
            val legacyRoot = JsonObject(
                root - setOf(
                    "lastBreakEndNotificationAttemptDigest",
                    "lastConsumedBreakEndNotificationDigest",
                    "lastRejectedBreakEndNotificationDigest",
                    "acknowledgedBreakEndDigest",
                ),
            )
            dao.snapshot = current.copy(
                payload = Json.encodeToString(JsonObject.serializer(), legacyRoot),
                payloadFormat = PlannerSnapshotFormats.JSON_V8,
                updatedAtEpochMillis = 3,
            )

            val restored = requireNotNull(repository.load())

            assertEquals(null, restored.lastBreakEndNotificationAttemptDigest)
            assertEquals(null, restored.lastConsumedBreakEndNotificationDigest)
            assertEquals(null, restored.lastRejectedBreakEndNotificationDigest)
            assertEquals(null, restored.acknowledgedBreakEndDigest)
            assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
            assertTrue(requireNotNull(dao.snapshot).payload.contains(
                "\"lastBreakEndNotificationAttemptDigest\":null",
            ))
        }

    @Test
    fun v8AndPredecessorLabelsDiscardInjectedNotificationReceipts() = runBlocking {
        val injectedDigest = "sha256:${"d".repeat(64)}"
        listOf(
            PlannerSnapshotFormats.JSON_V8,
            PlannerSnapshotFormats.JSON_V7,
        ).forEach { predecessorFormat ->
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { 6 }
            repository.save(
                DayWeaveUiState(
                    lastBreakEndNotificationAttemptDigest = injectedDigest,
                    lastConsumedBreakEndNotificationDigest = injectedDigest,
                    lastRejectedBreakEndNotificationDigest = injectedDigest,
                    acknowledgedBreakEndDigest = injectedDigest,
                ),
            )
            dao.snapshot = requireNotNull(dao.snapshot).copy(payloadFormat = predecessorFormat)

            val restored = requireNotNull(repository.load())

            assertEquals(null, restored.lastBreakEndNotificationAttemptDigest)
            assertEquals(null, restored.lastConsumedBreakEndNotificationDigest)
            assertEquals(null, restored.lastRejectedBreakEndNotificationDigest)
            assertEquals(null, restored.acknowledgedBreakEndDigest)
            assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        }
    }

    @Test
    fun v13MissingAnySafetyReceiptFailsClosedWithoutRewritingFixture() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 8 }
        repository.save(DayWeaveUiState())
        val current = requireNotNull(dao.snapshot)
        val root = Json.parseToJsonElement(current.payload) as JsonObject
        val missingReceipt = current.copy(
            payload = Json.encodeToString(
                JsonObject.serializer(),
                JsonObject(root - "lastConsumedBreakEndNotificationDigest"),
            ),
            updatedAtEpochMillis = 7,
        )
        dao.snapshot = missingReceipt

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(missingReceipt, dao.snapshot)
    }

    @Test
    fun malformedTimedBreakNotificationReceiptsAreDroppedIndependentlyAndRewritten() = runBlocking {
        val validAttemptDigest = "sha256:${"a".repeat(64)}"
        val validConsumedDigest = "sha256:${"b".repeat(64)}"
        val validAcknowledgedDigest = "sha256:${"c".repeat(64)}"
        val validRejectedDigest = "sha256:${"e".repeat(64)}"
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 5 }
        repository.save(
            DayWeaveUiState(
                lastBreakEndNotificationAttemptDigest = validAttemptDigest,
                lastConsumedBreakEndNotificationDigest = validConsumedDigest,
                lastRejectedBreakEndNotificationDigest = validRejectedDigest,
                acknowledgedBreakEndDigest = validAcknowledgedDigest,
            ),
        )
        val stored = requireNotNull(dao.snapshot)
        dao.snapshot = stored.copy(
            payload = stored.payload
                .replace(validAttemptDigest, "malformed-notification-attempt")
                .replace(validRejectedDigest, "malformed-notification-rejection")
                .replace(validAcknowledgedDigest, "malformed-notification-acknowledgement"),
            updatedAtEpochMillis = 6,
        )

        val restored = requireNotNull(repository.load())

        assertEquals(null, restored.lastBreakEndNotificationAttemptDigest)
        assertEquals(validConsumedDigest, restored.lastConsumedBreakEndNotificationDigest)
        assertEquals(null, restored.lastRejectedBreakEndNotificationDigest)
        assertEquals(null, restored.acknowledgedBreakEndDigest)
        assertFalse(requireNotNull(dao.snapshot).payload.contains("malformed-notification"))
        assertTrue(requireNotNull(dao.snapshot).payload.contains(validConsumedDigest))
        assertEquals(5L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun legacyV2PayloadDefaultsSensitivityAndIsRewrittenAsV13() = runBlocking {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 7,
                payloadFormat = PlannerSnapshotFormats.JSON_V2,
            ),
        )
        val repository = RoomPlannerStateRepository(dao) { 11 }

        val restored = requireNotNull(repository.load())

        assertFalse(restored.schedule.single().isSensitive)
        assertFalse(restored.canonicalItems.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        assertEquals(11L, dao.snapshot?.updatedAtEpochMillis)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":false"))
    }

    @Test
    fun sensitiveCanarySurvivesEncryptedSnapshotRoundTrip() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 13 }
        val state = DayWeaveUiState(
            schedule = listOf(
                ScheduleItem(
                    id = "SYNTHETIC-SENSITIVE-BLOCK-ANDROID",
                    isSensitive = true,
                    title = "SYNTHETIC-SENSITIVE-BLOCK-TITLE",
                    kind = ItemKind.TASK,
                    startMinute = 540,
                    durationMinutes = 30,
                    status = ItemStatus.SCHEDULED,
                ),
            ),
            canonicalItems = listOf(sensitiveCanonicalItem()),
            inbox = listOf(
                InboxItem(
                    id = "SYNTHETIC-SENSITIVE-INBOX-ANDROID",
                    isSensitive = true,
                    title = "SYNTHETIC-SENSITIVE-INBOX-TITLE",
                    source = InboxSource.QUICK_CAPTURE,
                ),
            ),
        )

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertTrue(restored.schedule.single().isSensitive)
        assertTrue(restored.canonicalItems.single().isSensitive)
        assertTrue(restored.inbox.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":true"))
    }

    @Test
    fun deferredExecutionHistorySurvivesEncryptedSnapshotRoundTrip() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 17 }
        val deferred = CanonicalExecutionSessionSnapshot(
            id = "44444444-4444-4444-8444-444444444444",
            itemId = "11111111-1111-4111-8111-111111111111",
            itemRevision = 7,
            sessionIndex = 0,
            plannedBlockId = "22222222-2222-4222-8222-222222222222",
            sourceDeviceId = "33333333-3333-4333-8333-333333333333",
            status = "deferred",
            revision = 2,
            accumulatedSeconds = 135,
            actualSeconds = 135,
            startedAt = "2026-09-01T06:45:00Z",
            endedAt = "2026-09-01T07:00:00Z",
            moveStart = "2026-09-01T08:00:00Z",
            moveEnd = "2026-09-01T09:00:00Z",
            createdAt = "2026-09-01T06:45:00Z",
            updatedAt = "2026-09-01T07:00:00Z",
        )
        val state = DayWeaveUiState(
            canonicalExecutionSyncOrigin = "https://api.example.test/",
            canonicalExecutionRevision = 2,
            canonicalExecutionHistoryWindow = listOf(deferred),
            canonicalExecutionHistoryWindowRevision = 2,
            canonicalExecutionHistoryContinuityEstablished = true,
            canonicalExecutionHistoryVerified = true,
            terminalExecutionOutcomes = mapOf(
                deferred.id to TerminalExecutionOutcomeSnapshot(
                    syncOrigin = "https://api.example.test/",
                    session = deferred,
                    requiresCanonicalItemProjection = false,
                    recordedAt = requireNotNull(deferred.endedAt),
                ),
            ),
        )

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(listOf(deferred), restored.canonicalExecutionHistoryWindow)
        assertEquals(2L, restored.canonicalExecutionHistoryWindowRevision)
        assertTrue(restored.canonicalExecutionHistoryVerified)
        val retained = restored.terminalExecutionOutcomes.getValue(deferred.id)
        assertEquals(deferred, retained.session)
        assertFalse(retained.requiresCanonicalItemProjection)
        assertEquals(deferred.endedAt, retained.recordedAt)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"moveStart\":"))
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"moveEnd\":"))
    }

    @Test
    fun recurrenceMoveSourceEnvelopeSurvivesEncryptedSnapshotRoundTrip() = runBlocking {
        val occurrenceId = "66666666-6666-5666-8666-666666666666"
        val itemId = "11111111-1111-4111-8111-111111111111"
        val source = RecurrenceOccurrenceSourceSnapshot(
            itemId = itemId,
            itemRevision = 7,
            identityJson =
                """{"type":"calendar_day","date":"2026-09-01","bucket_ordinal":0}""",
            nominalStart = "2026-09-01T09:00:00+02:00",
            nominalEnd = "2026-09-01T10:00:00+02:00",
            localDate = "2026-09-01",
            ordinal = 0,
        )
        val move = RecurrenceMoveSnapshot(
            itemId = itemId,
            startAt = "2026-09-03T10:00:00Z",
            endAt = "2026-09-03T11:00:00Z",
            movedAt = "2026-09-01T07:05:00Z",
            source = source,
        )
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao)
        val base = publishedScheduleState()
        val state = base.copy(
            canonicalItems = base.canonicalItems.map { item ->
                item.copy(recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}")
            },
            recurrenceMoves = mapOf(occurrenceId to move),
            occurrenceSeriesItemIds = mapOf(occurrenceId to itemId),
            recurrenceOccurrenceSources = mapOf(occurrenceId to source),
        )

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(state.recurrenceMoves, restored.recurrenceMoves)
        assertEquals(state.recurrenceOccurrenceSources, restored.recurrenceOccurrenceSources)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"nominalStart\":"))
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"itemRevision\":7"))
        assertTrue(requireNotNull(dao.snapshot).payload.contains("calendar_day"))
        assertEquals("2026-09-01T09:00:00+02:00", restored.recurrenceMoves
            .getValue(occurrenceId).source?.nominalStart)
        val relaunched = PlannerStore(restored).state.value
        assertEquals(move, relaunched.recurrenceMoves[occurrenceId])
        assertEquals(source, relaunched.recurrenceOccurrenceSources[occurrenceId])
    }

    @Test
    fun pendingExecutionDeferIntentSurvivesSerializedRepositoryAndStoreRelaunch() = runBlocking {
        val reference = Instant.parse("2026-08-29T08:30:00Z").toEpochMilli()
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { reference }
        val expected = pendingExecutionDeferState()

        repository.save(expected)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("pendingExecutionDeferIntent"))
        assertEquals(
            expected.pendingExecutionDeferIntent,
            requireNotNull(repository.load()).pendingExecutionDeferIntent,
        )

        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        try {
            val relaunched = PlannerStore(
                repository = RoomPlannerStateRepository(dao) { reference },
                scope = scope,
                nowEpochMillis = { reference },
            )
            withTimeout(3_000) {
                relaunched.loadState.first { it == PlannerLoadState.READY }
            }

            assertEquals(
                expected.pendingExecutionDeferIntent,
                relaunched.state.value.pendingExecutionDeferIntent,
            )
            assertTrue(relaunched.hasCredentialReplacementBlocker())
        } finally {
            scope.cancel()
        }
    }

    @Test
    fun authoritativeDeferAssessmentAndExactApprovalSurviveEncryptedSnapshotRoundTrip() =
        runBlocking {
            val reference = Instant.parse("2026-08-29T08:30:00Z").toEpochMilli()
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { reference }
            val base = pendingExecutionDeferState()
            val intent = requireNotNull(base.pendingExecutionDeferIntent)
            val assessmentDigest = "sha256:${"b".repeat(64)}"
            val assessment = ExecutionDeferAssessmentSnapshot(
                sessionId = intent.sessionId,
                executionRevision = base.canonicalExecutionRevision,
                sessionRevision = requireNotNull(base.canonicalExecutionSession).revision,
                itemId = intent.itemId,
                itemRevision = intent.itemRevision,
                occurrenceId = intent.occurrenceId,
                sourceSessionIndex = intent.sessionIndex,
                replacementSessionIndex = intent.sessionIndex + 1,
                sourceScheduleRevisionId = requireNotNull(base.publishedScheduleRevision).id,
                sourceBlockId = intent.plannedBlockId,
                actualSeconds = 300,
                creditedSourceSeconds = 300,
                plannedDurationSeconds = 1_800,
                remainingDurationSeconds = 1_500,
                moveStart = intent.moveStart,
                moveEnd = "2026-08-29T10:25:00Z",
                environmentDigest = "sha256:${"a".repeat(64)}",
                assessmentDigest = assessmentDigest,
                approvalRequired = true,
                violations = listOf(
                    ExecutionDeferViolationSnapshot(
                        code = "outside_availability",
                        itemIds = listOf(intent.itemId),
                        occurrenceIds = emptyList(),
                        conflictingBlockIds = emptyList(),
                        conflictingBlocks = emptyList(),
                        start = intent.moveStart,
                        end = "2026-08-29T10:25:00Z",
                        message =
                            "The requested placement is outside an allowed availability window.",
                    ),
                ),
                expiresAt = "2026-08-29T08:35:00Z",
            )
            val expected = base.copy(
                pendingExecutionDeferIntent = intent.copy(
                    assessment = assessment,
                    approvedAssessmentDigest = assessmentDigest,
                ),
            )

            repository.save(expected)
            val serialized = requireNotNull(dao.snapshot).payload
            assertTrue(serialized.contains("\"assessment_digest\""))
            assertTrue(serialized.contains(assessmentDigest))
            val restored = requireNotNull(repository.load())
            assertEquals(expected.pendingExecutionDeferIntent, restored.pendingExecutionDeferIntent)
            val relaunched = PlannerStore(
                restored,
                nowEpochMillis = { reference },
            ).state.value
            assertEquals(
                assessmentDigest,
                relaunched.pendingExecutionDeferIntent?.approvedAssessmentDigest,
            )
            assertEquals(assessment, relaunched.pendingExecutionDeferIntent?.assessment)
        }

    @Test
    fun malformedRestoredDeferIntentIsDurablyNormalizedThroughSerializedRepository() =
        runBlocking {
            val reference = Instant.parse("2026-08-29T08:30:00Z").toEpochMilli()
            val dao = FakePlannerSnapshotDao()
            val repository = RoomPlannerStateRepository(dao) { reference }
            val malformed = pendingExecutionDeferState().let { state ->
                state.copy(
                    pendingExecutionDeferIntent = requireNotNull(
                        state.pendingExecutionDeferIntent,
                    ).copy(moveStart = "not-an-instant"),
                )
            }
            repository.save(malformed)
            val seedSaveCount = dao.saveCount
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

            try {
                val relaunched = PlannerStore(
                    repository = RoomPlannerStateRepository(dao) { reference },
                    scope = scope,
                    nowEpochMillis = { reference },
                )
                withTimeout(3_000) {
                    relaunched.loadState.first { it == PlannerLoadState.READY }
                }
                assertEquals(null, relaunched.state.value.pendingExecutionDeferIntent)
                assertEquals("paused", relaunched.state.value.canonicalExecutionSession?.status)
                assertTrue(relaunched.state.value.scheduleMessage.contains("abandoned safely"))

                withTimeout(3_000) {
                    while (dao.saveCount <= seedSaveCount) delay(1)
                }
                val durable = requireNotNull(
                    RoomPlannerStateRepository(dao) { reference }.load(),
                )
                assertEquals(null, durable.pendingExecutionDeferIntent)
                assertEquals("paused", durable.canonicalExecutionSession?.status)
                assertTrue(durable.scheduleMessage.contains("abandoned safely"))
            } finally {
                scope.cancel()
            }
        }

    @Test
    fun exactSchedulePublicationJournalRoundTripsAndTamperingFailsClosed() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao)
        val state = pendingPublicationState()

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(
            state.pendingSchedulePublication,
            restored.pendingSchedulePublication,
        )
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)

        val digest = "sha256:${"a".repeat(64)}"
        val tampered = requireNotNull(dao.snapshot).payload.replaceFirst(
            digest,
            "sha256:${"b".repeat(64)}",
        )
        dao.snapshot = requireNotNull(dao.snapshot).copy(payload = tampered)

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun exactSchedulePublicationProofRoundTripsAndBlockTamperingFailsClosed() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 23 }
        val state = publishedScheduleState()

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(state.publishedScheduleProof, restored.publishedScheduleProof)
        assertTrue(
            requireNotNull(restored.publishedScheduleProof)
                .matches(restored.schedule.single()),
        )
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)

        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = requireNotNull(dao.snapshot).payload.replaceFirst(
                "2026-08-29T09:00:00Z",
                "2026-08-29T09:05:00Z",
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun legacyPublicationProofRoundTripsForMigrationButRemainsReadOnly() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 24 }
        val current = publishedScheduleState()
        val currentProof = requireNotNull(current.publishedScheduleProof)
        val legacyProof = currentProof.copy(
            schemaVersion = 1,
            blocks = currentProof.blocks.map { it.copy(immutableDigest = null) },
        )

        repository.save(current.copy(publishedScheduleProof = legacyProof))
        val restored = requireNotNull(repository.load())
        val restoredProof = requireNotNull(restored.publishedScheduleProof)
        val block = restored.schedule.single()
        val reference = Instant.parse("2026-08-29T12:00:00Z")
        val zone = ZoneId.of("UTC")

        assertEquals(legacyProof, restoredProof)
        assertTrue(restoredProof.hasValidShape())
        assertTrue(restoredProof.matchesStateBinding(restored))
        assertTrue(restoredProof.matchesPublishedPlan(restored.schedule))
        assertFalse(restoredProof.hasCurrentImmutablePlanSeal())
        assertFalse(restoredProof.matches(block))
        assertFalse(restored.isCanonicalPlanCurrent(reference, zone))
        assertFalse(restored.isPublishedScheduleDisplayCurrent(reference, zone))
        assertFalse(restored.hasPublishedExecutionAuthority(block))
    }

    @Test
    fun exactSchedulePublicationProofRequiresTheWholePublishedBlockSet() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 27 }
        val state = publishedScheduleState()
        val published = state.schedule.single()
        val extraPublished = published.copy(
            id = "55555555-5555-4555-8555-555555555555",
            sessionIndex = 3,
            absoluteStartAt = "2026-08-29T10:00:00Z",
            absoluteEndAt = "2026-08-29T10:30:00Z",
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.save(state.copy(schedule = listOf(published, extraPublished))) }
        }

        val localHelper = ScheduleItem(
            id = "local-helper",
            title = "Visible local helper",
            kind = ItemKind.ROUTINE,
            startMinute = 12 * 60,
            durationMinutes = 15,
            status = ItemStatus.SCHEDULED,
        )
        val remoteLease = published.copy(
            id = "66666666-6666-4666-8666-666666666666",
            status = ItemStatus.ACTIVE,
            canonicalBlockKind = "remote_execution_lease",
            absoluteStartAt = "2026-08-29T10:00:00Z",
            absoluteEndAt = null,
        )
        repository.save(state.copy(schedule = listOf(published, localHelper, remoteLease)))

        val restored = requireNotNull(repository.load())
        assertEquals(listOf(published, localHelper, remoteLease), restored.schedule)
        assertTrue(
            requireNotNull(restored.publishedScheduleProof)
                .matchesPublishedPlan(restored.schedule),
        )
    }

    @Test
    fun fullPlanProofRoundTripsExternalContextAndRejectsImmutableDisplayTampering() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 28 }
        val base = publishedScheduleState()
        val external = ScheduleItem(
            id = "77777777-7777-4777-8777-777777777777",
            isSensitive = true,
            title = "Private calendar hold",
            kind = ItemKind.EVENT,
            startMinute = 10 * 60,
            durationMinutes = 30,
            status = ItemStatus.SCHEDULED,
            isFlexible = false,
            isHardConstraint = true,
            sessionIndex = 0,
            absoluteStartAt = "2026-08-29T10:00:00Z",
            absoluteEndAt = "2026-08-29T10:30:00Z",
            planningZoneId = "UTC",
            canonicalBlockKind = "external_fixed",
        )
        val proof = requireNotNull(base.publishedScheduleProof).copy(
            blocks = (
                requireNotNull(base.publishedScheduleProof).blocks +
                    PublishedScheduleBlockProofSnapshot.from(external)
                ).sortedBy { it.id },
        )
        val state = base.copy(
            schedule = base.schedule + external,
            publishedScheduleProof = proof,
        )

        repository.save(state)
        val restored = requireNotNull(repository.load())

        assertEquals(state, restored)
        assertTrue(proof.matchesPublishedPlan(restored.schedule))
        assertThrows(SerializationException::class.java) {
            runBlocking {
                repository.save(
                    restored.copy(
                        schedule = restored.schedule.map { block ->
                            if (block.id == external.id) {
                                block.copy(isHardConstraint = false)
                            } else {
                                block
                            }
                        },
                    ),
                )
            }
        }
        Unit
    }

    @Test
    fun v7MigrationDiscardsEvenInjectedExactPublicationProof() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 29 }
        repository.save(publishedScheduleState())
        assertTrue(requireNotNull(dao.snapshot).payload.contains("publishedScheduleProof"))
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payloadFormat = PlannerSnapshotFormats.JSON_V7,
        )

        val restored = requireNotNull(repository.load())

        assertEquals(null, restored.publishedScheduleProof)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"publishedScheduleProof\":null"))
    }

    @Test
    fun exactProposalApplyJournalRoundTripsAndEndpointTamperingFailsClosed() = runBlocking {
        val configuration = AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "synthetic-token",
            "connection-1",
        )
        val proposalId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        val previewId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        val reviewHash = "sha256:${"c".repeat(64)}"
        val pending = PendingProposalApplicationMutation(
            schemaVersion = 1,
            kind = ProposalApplicationMutationKind.APPLY,
            idempotencyKey = "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = "connection-1",
            proposalId = proposalId,
            expectedProposalRevision = 4,
            expectedCommandIds = listOf("dddddddd-dddd-4ddd-8ddd-dddddddddddd"),
            previewId = previewId,
            expectedReviewHash = reviewHash,
            preparedAt = "2026-08-30T10:00:00Z",
            request = prepareProposalApplyHttpRequest(configuration, previewId, reviewHash),
        )
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao)

        repository.save(DayWeaveUiState(pendingProposalApplicationMutation = pending))
        assertEquals(
            pending,
            requireNotNull(repository.load()).pendingProposalApplicationMutation,
        )

        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = requireNotNull(dao.snapshot).payload.replaceFirst(
                "/application-previews/$previewId/apply",
                "/application-previews/$previewId/undo",
            ),
        )
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        Unit
    }

    @Test
    fun v5MigrationCreatesNoProposalApplicationState() = runBlocking {
        val dao = FakePlannerSnapshotDao()
        val repository = RoomPlannerStateRepository(dao) { 41 }
        repository.save(DayWeaveUiState())
        val root = Json.parseToJsonElement(requireNotNull(dao.snapshot).payload)
            .let { it as JsonObject }
        val legacy = JsonObject(
            root.filterKeys {
                it != "pendingProposalApplicationMutation" && it != "proposalApplications"
            },
        )
        dao.snapshot = requireNotNull(dao.snapshot).copy(
            payload = Json.encodeToString(JsonObject.serializer(), legacy),
            payloadFormat = PlannerSnapshotFormats.JSON_V5,
        )

        val restored = requireNotNull(repository.load())

        assertEquals(null, restored.pendingProposalApplicationMutation)
        assertTrue(restored.proposalApplications.isEmpty())
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
    }

    @Test
    fun exactUndoJournalRoundTripsOnlyWithItsMatchingAppliedReceipt() = runBlocking {
        val configuration = AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "synthetic-token",
            "connection-1",
        )
        val proposalId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        val applicationId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        val commandId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        val receipt = ProposalApplicationReceiptSnapshot(
            schemaVersion = 1,
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = "connection-1",
            applicationId = applicationId,
            proposalId = proposalId,
            appliedProposalRevision = 2,
            applicationRevision = 1,
            status = ProposalApplicationStatusSnapshot.APPLIED,
            commandIds = listOf(commandId),
            affectedItemIds = listOf("dddddddd-dddd-4ddd-8ddd-dddddddddddd"),
            appliedAt = "2026-08-30T10:00:00Z",
            undoExpiresAt = "2026-08-30T10:15:00Z",
        )
        val pending = PendingProposalApplicationMutation(
            schemaVersion = 1,
            kind = ProposalApplicationMutationKind.UNDO,
            idempotencyKey = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            syncOrigin = configuration.baseUrl.toString(),
            configurationId = "connection-1",
            proposalId = proposalId,
            expectedProposalRevision = 2,
            expectedCommandIds = listOf(commandId),
            applicationId = applicationId,
            expectedApplicationRevision = 1,
            preparedAt = "2026-08-30T10:05:00Z",
            request = prepareProposalUndoHttpRequest(configuration, applicationId, 1),
        )
        val repository = RoomPlannerStateRepository(FakePlannerSnapshotDao())
        val state = DayWeaveUiState(
            pendingProposalApplicationMutation = pending,
            proposalApplications = mapOf(proposalId to receipt),
        )

        repository.save(state)
        assertEquals(pending, requireNotNull(repository.load()).pendingProposalApplicationMutation)
        assertThrows(SerializationException::class.java) {
            runBlocking { repository.save(state.copy(proposalApplications = emptyMap())) }
        }
        Unit
    }

    @Test
    fun currentV4PayloadMissingSensitivityFailsClosed() {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 17,
                payloadFormat = PlannerSnapshotFormats.JSON_V4,
            ),
        )
        val repository = RoomPlannerStateRepository(dao)

        assertThrows(SerializationException::class.java) {
            runBlocking { repository.load() }
        }
        assertEquals(PlannerSnapshotFormats.JSON_V4, dao.snapshot?.payloadFormat)
        assertEquals(17L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun legacyV3StillRejectsMissingPreviouslyRequiredSensitivity() {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V2_PAYLOAD,
                updatedAtEpochMillis = 18,
                payloadFormat = PlannerSnapshotFormats.JSON_V3,
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { RoomPlannerStateRepository(dao).load() }
        }
        assertEquals(PlannerSnapshotFormats.JSON_V3, dao.snapshot?.payloadFormat)
        assertEquals(18L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun legacyV3DerivesPendingSensitivityFromExactReplacementBody() = runBlocking {
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = LEGACY_V3_PENDING_PAYLOAD,
                updatedAtEpochMillis = 19,
                payloadFormat = PlannerSnapshotFormats.JSON_V3,
            ),
        )
        val repository = RoomPlannerStateRepository(dao) { 23 }

        val restored = requireNotNull(repository.load())

        assertTrue(requireNotNull(restored.pendingCanonicalMutation).targetIsSensitive)
        assertFalse(restored.inbox.single().isSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"targetIsSensitive\":true"))
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"isSensitive\":false"))
        assertTrue(requireNotNull(repository.load()).pendingCanonicalMutation?.targetIsSensitive == true)
    }

    @Test
    fun legacyV2PendingJournalWithoutPreexistingSensitivityMigratesExplicitlyFalse() = runBlocking {
        val preSensitivityPayload = LEGACY_V3_PENDING_PAYLOAD
            .replace("\"isSensitive\": true", "\"isSensitive\": false")
            .replace(",\\\"is_sensitive\\\":true", "")
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = preSensitivityPayload,
                updatedAtEpochMillis = 24,
                payloadFormat = PlannerSnapshotFormats.JSON_V2,
            ),
        )
        val repository = RoomPlannerStateRepository(dao) { 25 }

        val restored = requireNotNull(repository.load())

        assertFalse(requireNotNull(restored.pendingCanonicalMutation).targetIsSensitive)
        assertEquals(PlannerSnapshotFormats.JSON_V16, dao.snapshot?.payloadFormat)
        assertTrue(requireNotNull(dao.snapshot).payload.contains("\"targetIsSensitive\":false"))
    }

    @Test
    fun currentV4PendingMutationMissingSensitivityTargetFailsClosed() {
        val currentPayload = LEGACY_V3_PENDING_PAYLOAD.replace(
            "\"source\": \"QUICK_CAPTURE\"",
            "\"source\": \"QUICK_CAPTURE\", \"isSensitive\": false",
        )
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = currentPayload,
                updatedAtEpochMillis = 29,
                payloadFormat = PlannerSnapshotFormats.JSON_V4,
            ),
        )

        assertThrows(SerializationException::class.java) {
            runBlocking { RoomPlannerStateRepository(dao).load() }
        }
        assertEquals(29L, dao.snapshot?.updatedAtEpochMillis)
    }

    @Test
    fun currentV4RejectsSensitivityTargetThatDisagreesWithWireJournal() {
        val currentPayload = LEGACY_V3_PENDING_PAYLOAD
            .replace(
                "\"source\": \"QUICK_CAPTURE\"",
                "\"source\": \"QUICK_CAPTURE\", \"isSensitive\": false",
            )
            .replace(
                "\"targetStatus\": \"planned\",",
                "\"targetStatus\": \"planned\", \"targetIsSensitive\": false,",
            )
        val dao = FakePlannerSnapshotDao(
            PlannerSnapshotEntity(
                singletonId = 1,
                payload = currentPayload,
                updatedAtEpochMillis = 31,
                payloadFormat = PlannerSnapshotFormats.JSON_V4,
            ),
        )

        val failure = assertThrows(SerializationException::class.java) {
            runBlocking { RoomPlannerStateRepository(dao).load() }
        }
        assertTrue(requireNotNull(failure.message).contains("does not match its exact replacement"))
        assertEquals(31L, dao.snapshot?.updatedAtEpochMillis)
    }

    private fun outboundState(journal: GoogleCalendarOutboundJournal) = DayWeaveUiState(
        canonicalSyncOrigin = OUTBOUND_API_BASE_URL,
        canonicalConfigurationId = OUTBOUND_CONFIGURATION_ID,
        canonicalDeltaCursor = "cursor-1",
        pendingGoogleCalendarOutbound = journal,
    )

    private fun validOutboundJournal(
        stage: GoogleCalendarOutboundStage = GoogleCalendarOutboundStage.APPROVED,
        createdAt: String = "2026-09-02T12:00:00Z",
        intentExpiresAt: String = "2026-09-02T12:30:00Z",
        previewExpiresAt: String = "2026-09-02T12:20:00Z",
        approvalExpiresAt: String = "2026-09-02T12:15:00Z",
        entityKind: GoogleCalendarOutboundEntityKind =
            GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
        operation: GoogleCalendarOutboundOperation = GoogleCalendarOutboundOperation.UPSERT,
    ): GoogleCalendarOutboundJournal {
        val intent = GoogleCalendarOutboundJournal(
            recoveryId = "11111111-1111-4111-8111-111111111111",
            operationGeneration = 1,
            configurationId = OUTBOUND_CONFIGURATION_ID,
            apiBaseUrl = OUTBOUND_API_BASE_URL,
            accountId = "22222222-2222-4222-8222-222222222222",
            collectionId = "33333333-3333-4333-8333-333333333333",
            itemId = "44444444-4444-4444-8444-444444444444",
            expectedItemRevision = 7,
            entityKind = entityKind,
            operation = operation,
            intentExpiresAt = intentExpiresAt,
            createdAt = createdAt,
        )
        if (stage == GoogleCalendarOutboundStage.INTENT) return intent
        val previewed = intent.recordingPreview(
            GoogleCalendarOutboundPreviewSnapshot(
                id = "55555555-5555-4555-8555-555555555555",
                accountId = intent.accountId,
                collectionId = intent.collectionId,
                collectionRevision = 4,
                collectionDisplayName = "Private calendar",
                itemId = intent.itemId,
                itemRevision = intent.expectedItemRevision,
                entityKind = entityKind,
                operation = operation,
                providerResourceId = if (operation == GoogleCalendarOutboundOperation.DELETE) {
                    "provider-resource-1"
                } else {
                    null
                },
                providerEtag = if (operation == GoogleCalendarOutboundOperation.DELETE) {
                    "provider-etag-1"
                } else {
                    null
                },
                previewHash = "a".repeat(64),
                providerPayload = when (operation) {
                    GoogleCalendarOutboundOperation.DELETE -> JsonObject(emptyMap())
                    GoogleCalendarOutboundOperation.UPSERT -> when (entityKind) {
                        GoogleCalendarOutboundEntityKind.CALENDAR_EVENT ->
                            validOutboundProviderPayload()
                        GoogleCalendarOutboundEntityKind.TASK -> validOutboundTaskPayload()
                    }
                },
                expiresAt = previewExpiresAt,
            ),
        )
        if (stage == GoogleCalendarOutboundStage.PREVIEWED) return previewed
        val attempted = previewed.recordingApprovalAttempt()
        if (stage == GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED) return attempted
        return attempted.recordingApproval(
            RemoteGoogleOutboundApproval(
                previewId = requireNotNull(attempted.preview).id,
                approvalCapability = OUTBOUND_CAPABILITY,
                expiresAt = approvalExpiresAt,
            ),
        )
    }

    private fun validOutboundTaskPayload(): JsonObject = JsonObject(
        mapOf(
            "id" to JsonPrimitive(""),
            "etag" to JsonNull,
            "title" to JsonPrimitive("Private task"),
            "notes" to JsonPrimitive("Private notes"),
            "status" to JsonPrimitive("needsAction"),
            "due" to JsonPrimitive("2026-09-03T12:00:00Z"),
            "completed" to JsonNull,
            "updated" to JsonNull,
            "parent" to JsonNull,
            "position" to JsonNull,
            "links" to JsonNull,
            "deleted" to JsonPrimitive(false),
            "hidden" to JsonPrimitive(false),
        ),
    )

    private fun validOutboundProviderPayload(): JsonObject {
        fun boundary(dateTime: String) = JsonObject(
            mapOf(
                "date" to JsonNull,
                "dateTime" to JsonPrimitive(dateTime),
                "timeZone" to JsonPrimitive("Europe/Paris"),
            ),
        )
        return JsonObject(
            mapOf(
                "id" to JsonPrimitive("d1" + "a".repeat(64)),
                "etag" to JsonNull,
                "summary" to JsonPrimitive("Private focus"),
                "description" to JsonPrimitive("Private notes"),
                "location" to JsonNull,
                "status" to JsonPrimitive("confirmed"),
                "transparency" to JsonPrimitive("opaque"),
                "visibility" to JsonPrimitive("private"),
                "eventType" to JsonPrimitive("default"),
                "start" to boundary("2026-09-02T10:00:00+02:00"),
                "end" to boundary("2026-09-02T11:00:00+02:00"),
                "attendees" to JsonArray(emptyList()),
                "attachments" to JsonArray(emptyList()),
                "recurrence" to JsonArray(emptyList()),
                "conferenceData" to JsonNull,
                "recurringEventId" to JsonNull,
                "originalStartTime" to JsonNull,
                "updated" to JsonNull,
                "sequence" to JsonNull,
                "extendedProperties" to JsonObject(
                    mapOf(
                        "private" to JsonObject(
                            mapOf(
                                "dayweaveOwnershipProof" to JsonPrimitive(OWNERSHIP_PROOF),
                            ),
                        ),
                        "shared" to JsonObject(emptyMap()),
                    ),
                ),
            ),
        )
    }

    private fun localProvenanceState(
        horizonDays: Long = 7,
        profileDays: Int = 7,
    ): DayWeaveUiState {
        val origin = "https://api.example.test/"
        val configurationId = "connection-1"
        val generatedAt = "2026-08-29T08:00:00Z"
        val base = DayWeaveUiState(
            scheduleCompositionProfile = ScheduleCompositionProfileSnapshot(
                firmHorizonDays = profileDays,
            ),
            canonicalSyncOrigin = origin,
            canonicalConfigurationId = configurationId,
            canonicalDeltaCursor = "cursor-1",
            canonicalExecutionSyncOrigin = origin,
            canonicalExecutionConfigurationId = configurationId,
            canonicalExecutionHistoryWindowRevision = 0,
            canonicalExecutionHistoryContinuityEstablished = true,
            canonicalExecutionHistoryVerified = true,
            scheduleGeneratedAt = generatedAt,
            schedulePlanningZoneId = "UTC",
        )
        val provenance = LocalScheduleCompositionProvenanceSnapshot(
            syncOrigin = origin,
            configurationId = configurationId,
            deltaCursor = "cursor-1",
            localInputFingerprint = "local-sha256:${"a".repeat(64)}",
            scheduleRequestFingerprint = "sha256:${"b".repeat(64)}",
            stateInputFingerprint = base.localScheduleCompositionStateFingerprint(),
            generatedAt = generatedAt,
            asOf = generatedAt,
            horizonStart = "2026-08-29T00:00:00Z",
            horizonEnd = Instant.parse("2026-08-29T00:00:00Z")
                .plusSeconds(horizonDays * 24 * 60 * 60)
                .toString(),
            timezoneName = "UTC",
            sourceItemRevisions = emptyMap(),
        )
        return base.copy(localScheduleCompositionProvenance = provenance)
    }

    private fun sensitiveCanonicalItem() = CanonicalItemSnapshot(
        id = "SYNTHETIC-SENSITIVE-CANONICAL-ANDROID",
        isSensitive = true,
        kind = "task",
        status = "planned",
        title = "SYNTHETIC-SENSITIVE-CANONICAL-TITLE",
        timezoneName = "UTC",
        durationSeconds = 1_800,
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = "2026-08-29T08:00:00Z",
        updatedAt = "2026-08-29T08:00:00Z",
    )

    private fun pendingPublicationState(): DayWeaveUiState {
        val origin = "https://api.example.test/"
        val configurationId = "connection-1"
        val idempotencyKey = "33333333-3333-4333-8333-333333333333"
        val digest = "sha256:${"a".repeat(64)}"
        val schedule = SchedulePreviewRequest(
            asOf = "2026-08-29T08:00:00Z",
            horizonStart = "2026-08-29T00:00:00Z",
            horizonEnd = "2026-08-30T00:00:00Z",
            timezoneName = "UTC",
            availability = listOf(
                ScheduleAvailabilityRequest(
                    start = "2026-08-29T00:00:00Z",
                    end = "2026-08-30T00:00:00Z",
                ),
            ),
        )
        val candidate = CanonicalPlanUpdate(
            items = emptyList(),
            schedule = emptyList(),
            syncOrigin = origin,
            configurationId = configurationId,
            deltaCursor = "cursor-1",
            inputDigest = digest,
            generatedAt = schedule.asOf,
            planningZoneId = schedule.timezoneName,
            rejectedItemCount = 0,
            unscheduledItemCount = 0,
            protectedFreeMinutes = 0,
            dayScore = 100,
            violationMessages = emptyList(),
            violationCount = 0,
            errorViolationCount = 0,
            unscheduledWork = emptyList(),
            occurrenceSeriesItemIds = emptyMap(),
            message = "Synthetic pending publication",
        )
        val configuration = AuthenticatedApiConfiguration.createBound(
            origin,
            "synthetic-token",
            configurationId,
        )
        return DayWeaveUiState(
            pendingSchedulePublication = PendingSchedulePublication(
                schemaVersion = 1,
                idempotencyKey = idempotencyKey,
                syncOrigin = origin,
                configurationId = configurationId,
                preparedAt = schedule.asOf,
                request = buildSchedulePublishHttpRequest(
                    configuration,
                    SchedulePublishRequest(idempotencyKey, digest, schedule),
                ),
                candidate = candidate,
            ),
        )
    }

    private fun publishedScheduleState(): DayWeaveUiState {
        val origin = "https://api.example.test/"
        val configurationId = "connection-1"
        val itemId = "11111111-1111-4111-8111-111111111111"
        val blockId = "22222222-2222-4222-8222-222222222222"
        val digest = "sha256:${"b".repeat(64)}"
        val revision = PublishedScheduleRevisionSnapshot(
            id = "33333333-3333-4333-8333-333333333333",
            revision = "4:33333333-3333-4333-8333-333333333333",
            revisionNumber = 4uL,
            inputDigest = digest,
            horizonStart = "2026-08-29T00:00:00Z",
            horizonEnd = "2026-08-30T00:00:00Z",
            timezoneName = "UTC",
            publishedAt = "2026-08-29T08:00:00Z",
        )
        val block = ScheduleItem(
            id = blockId,
            title = "Published task",
            kind = ItemKind.TASK,
            startMinute = 9 * 60,
            durationMinutes = 30,
            status = ItemStatus.SCHEDULED,
            canonicalItemId = itemId,
            canonicalRevision = 7,
            sessionIndex = 2,
            absoluteStartAt = "2026-08-29T09:00:00Z",
            absoluteEndAt = "2026-08-29T09:30:00Z",
            planningZoneId = "UTC",
            canonicalBlockKind = "planned",
        )
        return DayWeaveUiState(
            schedule = listOf(block),
            canonicalItems = listOf(
                sensitiveCanonicalItem().copy(
                    id = itemId,
                    isSensitive = false,
                    title = block.title,
                    revision = 7,
                ),
            ),
            canonicalSyncOrigin = origin,
            canonicalConfigurationId = configurationId,
            canonicalDeltaCursor = "cursor-7",
            publishedScheduleRevision = revision,
            publishedScheduleProof = PublishedScheduleProofSnapshot(
                schemaVersion = PublishedScheduleProofSnapshot.CURRENT_SCHEMA_VERSION,
                syncOrigin = origin,
                configurationId = configurationId,
                revision = revision,
                asOf = "2026-08-29T08:00:00Z",
                blocks = listOf(
                    PublishedScheduleBlockProofSnapshot.from(block),
                ),
            ),
            scheduleInputDigest = digest,
            scheduleGeneratedAt = "2026-08-29T08:00:00Z",
            schedulePlanningZoneId = "UTC",
        )
    }

    private fun pendingExecutionDeferState(): DayWeaveUiState {
        val published = publishedScheduleState()
        val block = published.schedule.single().copy(status = ItemStatus.PAUSED)
        val session = CanonicalExecutionSessionSnapshot(
            id = EXECUTION_SESSION_ID,
            itemId = requireNotNull(block.canonicalItemId),
            itemRevision = requireNotNull(block.canonicalRevision),
            sessionIndex = requireNotNull(block.sessionIndex),
            plannedBlockId = block.id,
            sourceDeviceId = EXECUTION_DEVICE_ID,
            status = "paused",
            revision = 2,
            accumulatedSeconds = 300,
            startedAt = requireNotNull(block.absoluteStartAt),
            pausedAt = "2026-08-29T09:05:00Z",
            createdAt = requireNotNull(block.absoluteStartAt),
            updatedAt = "2026-08-29T09:05:00Z",
        )
        return published.copy(
            schedule = listOf(block),
            canonicalExecutionSyncOrigin = requireNotNull(published.canonicalSyncOrigin),
            canonicalExecutionConfigurationId = published.canonicalConfigurationId,
            canonicalExecutionRevision = session.revision,
            canonicalExecutionSession = session,
            pendingExecutionDeferIntent = PendingExecutionDeferIntent(
                schemaVersion = 1,
                syncOrigin = requireNotNull(published.canonicalSyncOrigin),
                configurationId = published.canonicalConfigurationId,
                sessionId = session.id,
                itemId = session.itemId,
                itemRevision = session.itemRevision,
                sessionIndex = session.sessionIndex,
                plannedBlockId = requireNotNull(session.plannedBlockId),
                sourceDeviceId = session.sourceDeviceId,
                focusedBlockId = block.id,
                sourceStart = requireNotNull(block.absoluteStartAt),
                sourceEnd = requireNotNull(block.absoluteEndAt),
                moveStart = "2026-08-29T10:00:00Z",
                stagedAt = "2026-08-29T09:05:00Z",
            ),
        )
    }

    private class FakePlannerSnapshotDao(
        var snapshot: PlannerSnapshotEntity? = null,
    ) : PlannerSnapshotDao {
        private val saveCounter = AtomicInteger(0)
        val saveCount: Int get() = saveCounter.get()

        override suspend fun load(singletonId: Int): PlannerSnapshotEntity? = snapshot

        override suspend fun save(snapshot: PlannerSnapshotEntity) {
            this.snapshot = snapshot
            saveCounter.incrementAndGet()
        }
    }

    private companion object {
        val CANONICAL_STRUCTURAL_TEST_FIELDS = setOf(
            "durationKind",
            "durationMinSeconds",
            "durationMaxSeconds",
            "durationSource",
            "deadlineKind",
            "deadlineDate",
            "deadlineStrength",
            "deadlineSoftWeight",
            "hasOwnEffort",
            "blockedReasonKind",
            "blockedByItemId",
            "blockedReason",
            "hasExplicitStructuralMetadata",
        )
        const val EXECUTION_SESSION_ID = "44444444-4444-4444-8444-444444444444"
        const val EXECUTION_DEVICE_ID = "55555555-5555-4555-8555-555555555555"
        const val OUTBOUND_API_BASE_URL = "https://api.example.test/"
        const val OUTBOUND_CONFIGURATION_ID = "connection-1"
        const val OUTBOUND_CAPABILITY =
            "dw_ga1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        const val OWNERSHIP_PROOF = "[server-managed]"
        const val LEGACY_V2_PAYLOAD = """
            {
              "schedule": [{
                "id": "SYNTHETIC-LEGACY-V2-BLOCK",
                "title": "SYNTHETIC-LEGACY-V2-BLOCK-TITLE",
                "kind": "TASK",
                "startMinute": 540,
                "durationMinutes": 30,
                "status": "SCHEDULED"
              }],
              "canonicalItems": [{
                "id": "SYNTHETIC-LEGACY-V2-CANONICAL",
                "kind": "task",
                "status": "planned",
                "title": "SYNTHETIC-LEGACY-V2-CANONICAL-TITLE",
                "timezoneName": "UTC",
                "durationSeconds": 1800,
                "flexibleConstraintsJson": "{}",
                "splitPolicyJson": "{\"type\":\"indivisible\"}",
                "importance": 50,
                "urgency": 50,
                "siblingOrder": 0,
                "isExecutable": true,
                "revision": 1,
                "createdAt": "2026-08-29T08:00:00Z",
                "updatedAt": "2026-08-29T08:00:00Z"
              }]
            }
        """

        const val LEGACY_V3_PENDING_PAYLOAD = """
            {
              "schedule": [],
              "canonicalItems": [{
                "id": "11111111-1111-4111-8111-111111111111",
                "isSensitive": true,
                "kind": "task",
                "status": "planned",
                "title": "SYNTHETIC-LEGACY-PENDING-CANONICAL",
                "timezoneName": "UTC",
                "durationSeconds": 1800,
                "flexibleConstraintsJson": "{}",
                "splitPolicyJson": "{\"type\":\"indivisible\"}",
                "importance": 50,
                "urgency": 50,
                "siblingOrder": 0,
                "isExecutable": true,
                "revision": 1,
                "createdAt": "2026-08-29T08:00:00Z",
                "updatedAt": "2026-08-29T08:00:00Z"
              }],
              "inbox": [{
                "id": "SYNTHETIC-LEGACY-INBOX",
                "title": "SYNTHETIC-LEGACY-INBOX-TITLE",
                "source": "QUICK_CAPTURE"
              }],
              "pendingCanonicalMutation": {
                "idempotencyKey": "22222222-2222-4222-8222-222222222222",
                "syncOrigin": "https://api.example.test/",
                "configurationId": "SYNTHETIC-CONNECTION",
                "itemId": "11111111-1111-4111-8111-111111111111",
                "expectedRevision": 1,
                "targetStatus": "planned",
                "startedAt": "2026-08-29T08:01:00Z",
                "replacementRequestJson": "{\"expected_revision\":1,\"item\":{\"status\":\"planned\",\"is_sensitive\":true}}",
                "focusedBlockId": "11111111-1111-4111-8111-111111111111",
                "displayStatus": "SCHEDULED"
              }
            }
        """
    }
}
