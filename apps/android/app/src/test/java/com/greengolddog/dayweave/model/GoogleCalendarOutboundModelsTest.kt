package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.GoogleCalendarOutboundEntityKind
import com.greengolddog.dayweave.network.GoogleCalendarOutboundOperation
import com.greengolddog.dayweave.network.RemoteGoogleCalendarPolicy
import com.greengolddog.dayweave.network.RemoteGoogleCalendarProjectionState
import com.greengolddog.dayweave.network.RemoteGoogleCollectionKind
import com.greengolddog.dayweave.network.RemoteGoogleOutboundApproval
import com.greengolddog.dayweave.network.RemoteGoogleOutboundPreview
import com.greengolddog.dayweave.network.RemoteGoogleSyncCollection
import com.greengolddog.dayweave.network.RemoteGoogleSyncRole
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonPrimitive

class GoogleCalendarOutboundModelsTest {
    @Test
    fun journalAllowsOnlyExactForwardTransitionsAndRedactsAuthority() {
        val intent = validIntent()
        val preview = intent.recordingPreview(validRemotePreview())
        val attempted = preview.recordingApprovalAttempt()
        val approved = attempted.recordingApproval(
            RemoteGoogleOutboundApproval(
                previewId = PREVIEW_ID,
                approvalCapability = CAPABILITY,
                expiresAt = "2026-09-02T12:14:00Z",
            ),
        )

        assertEquals(GoogleCalendarOutboundStage.INTENT, intent.stage)
        assertEquals(GoogleCalendarOutboundStage.PREVIEWED, preview.stage)
        assertEquals(GoogleCalendarOutboundStage.APPROVAL_ATTEMPTED, attempted.stage)
        assertEquals(GoogleCalendarOutboundStage.APPROVED, approved.stage)
        assertTrue(intent.canTransitionTo(preview))
        assertTrue(preview.canTransitionTo(attempted))
        assertTrue(attempted.canTransitionTo(approved))
        assertFalse(intent.canTransitionTo(attempted))
        assertFalse(preview.canTransitionTo(approved))
        assertFalse(approved.canTransitionTo(attempted))
        assertFalse(intent.canTransitionTo(preview.copy(operationGeneration = 2)))
        assertFalse(
            intent.canTransitionTo(
                intent.copy(entityKind = GoogleCalendarOutboundEntityKind.TASK),
            ),
        )
        assertFalse(
            intent.canTransitionTo(
                intent.copy(operation = GoogleCalendarOutboundOperation.DELETE),
            ),
        )
        assertEquals(Instant.parse("2026-09-02T12:14:00Z"), approved.authorityExpiresAt())
        assertEquals(Instant.parse("2026-09-02T12:19:00Z"), approved.safeDiscardAt())

        listOf(
            approved.toString(),
            requireNotNull(approved.preview).toString(),
            requireNotNull(approved.approvalCapability).toString(),
        ).forEach { diagnostic ->
            assertFalse(diagnostic.contains(ITEM_ID))
            assertFalse(diagnostic.contains(ACCOUNT_ID))
            assertFalse(diagnostic.contains(COLLECTION_ID))
            assertFalse(diagnostic.contains(PREVIEW_ID))
            assertFalse(diagnostic.contains(PREVIEW_HASH))
            assertFalse(diagnostic.contains(CAPABILITY))
            assertFalse(diagnostic.contains("Private focus"))
        }
    }

    @Test
    fun journalRejectsInvalidStageShapesAndLifetime() {
        assertThrows(IllegalArgumentException::class.java) {
            validIntent().copy(operationGeneration = 0)
        }
        assertThrows(IllegalArgumentException::class.java) {
            validIntent().copy(intentExpiresAt = "2026-09-02T12:35:01Z")
        }
        assertThrows(IllegalArgumentException::class.java) {
            validIntent().copy(approvalAttempted = true)
        }
        assertThrows(IllegalArgumentException::class.java) {
            validIntent().copy(
                approvalCapability = GoogleCalendarOutboundApprovalCapability(CAPABILITY),
                approvalExpiresAt = "2026-09-02T12:14:00Z",
            )
        }
        assertFalse(validIntent().isValidAt(Instant.parse("2026-09-02T11:54:59Z")))
        assertTrue(validIntent().isValidAt(Instant.parse("2026-09-02T11:55:00Z")))
    }

    @Test
    fun persistedPreviewRevalidatesExactBoundedPrivateFixedEvent() {
        val remote = validRemotePreview()
        assertEquals(
            GoogleCalendarOutboundPreviewSnapshot(
                id = remote.id,
                accountId = remote.accountId,
                collectionId = remote.collectionId,
                collectionRevision = remote.collectionRevision,
                collectionDisplayName = remote.collectionDisplayName,
                itemId = remote.itemId,
                itemRevision = remote.itemRevision,
                entityKind = remote.entityKind,
                operation = remote.operation,
                providerResourceId = remote.providerResourceId,
                providerEtag = remote.providerEtag,
                previewHash = remote.previewHash,
                providerPayload = remote.providerPayload,
                expiresAt = remote.expiresAt,
            ),
            GoogleCalendarOutboundPreviewSnapshot.fromRemote(remote),
        )

        val invalidPayloads = listOf(
            JsonObject(validPayload() + ("reminders" to JsonObject(emptyMap()))),
            JsonObject(validPayload() - "location"),
            validPayload().replacing("id", JsonPrimitive("provider-selected-id")),
            validPayload().replacing("etag", JsonPrimitive("provider-etag")),
            validPayload().replacing("visibility", JsonPrimitive("public")),
            validPayload().replacing("eventType", JsonPrimitive("focusTime")),
            validPayload().replacing("status", JsonPrimitive("tentative")),
            validPayload().replacing("transparency", JsonPrimitive("transparent")),
            JsonObject(
                validPayload() + mapOf(
                    "start" to allDayCalendarBoundary("2026-09-02"),
                    "end" to allDayCalendarBoundary("2026-09-03"),
                ),
            ),
            validPayload().replacing("attendees", JsonArray(listOf(JsonObject(emptyMap())))),
            validPayload().replacing("attachments", JsonArray(listOf(JsonObject(emptyMap())))),
            validPayload().replacing("recurrence", JsonArray(listOf(JsonPrimitive("RRULE:FREQ=DAILY")))),
            validPayload().replacing("conferenceData", JsonObject(emptyMap())),
            validPayload().replacing("recurringEventId", JsonPrimitive("provider-secret")),
            validPayload().replacing("originalStartTime", JsonObject(emptyMap())),
            validPayload().replacing("location", JsonPrimitive("Unreviewed room")),
            validPayload().replacing("updated", JsonPrimitive("2026-09-02T12:00:00Z")),
            validPayload().replacing("sequence", JsonPrimitive(1)),
            validPayload().replacing(
                "extendedProperties",
                JsonObject(
                    mapOf(
                        "private" to JsonObject(
                            mapOf("dayweaveOwnershipProof" to JsonPrimitive("invalid")),
                        ),
                        "shared" to JsonObject(emptyMap()),
                    ),
                ),
            ),
            validPayload().replacing("guestCanInviteOthers", JsonPrimitive(true)),
            validPayload().replacing(
                "summary",
                JsonPrimitive("x".repeat(8 * 1_024 + 1)),
            ),
            validPayload().replacing(
                "end",
                calendarBoundary("2026-09-02T09:59:59+02:00"),
            ),
        )
        invalidPayloads.forEachIndexed { index, payload ->
            assertThrows("payload case $index", IllegalArgumentException::class.java) {
                GoogleCalendarOutboundPreviewSnapshot.fromRemote(
                    remote.copy(providerPayload = payload),
                )
            }
        }
    }

    @Test
    fun taskUpsertRequiresExactInertProjectionAndDeleteRequiresEmptyPayload() {
        val task = validRemoteTaskPreview()
        val snapshot = GoogleCalendarOutboundPreviewSnapshot.fromRemote(task)
        assertEquals(GoogleCalendarOutboundEntityKind.TASK, snapshot.entityKind)
        assertEquals(GoogleCalendarOutboundOperation.UPSERT, snapshot.operation)
        assertEquals("Private task", snapshot.providerPayload["title"]?.jsonPrimitive?.content)
        assertEquals(
            GoogleCalendarOutboundStage.PREVIEWED,
            validIntent().copy(entityKind = GoogleCalendarOutboundEntityKind.TASK)
                .recordingPreview(task).stage,
        )

        val invalidPayloads = listOf(
            JsonObject(validTaskPayload() - "title"),
            JsonObject(validTaskPayload() + ("kind" to JsonPrimitive("tasks#task"))),
            validTaskPayload().replacing("id", JsonPrimitive("provider-task-id")),
            validTaskPayload().replacing("etag", JsonPrimitive("provider-etag")),
            validTaskPayload().replacing("title", JsonPrimitive("  Private task")),
            validTaskPayload().replacing("notes", JsonPrimitive("ordinary\n[DayWeave item:1]")),
            validTaskPayload().replacing("status", JsonPrimitive("cancelled")),
            validTaskPayload().replacing("completed", JsonNull),
            validTaskPayload().replacing("updated", JsonPrimitive("2026-09-02T12:00:00Z")),
            validTaskPayload().replacing("parent", JsonPrimitive("provider-parent")),
            validTaskPayload().replacing("position", JsonPrimitive("0001")),
            validTaskPayload().replacing("links", JsonArray(emptyList())),
            validTaskPayload().replacing("deleted", JsonPrimitive(true)),
            validTaskPayload().replacing("hidden", JsonPrimitive(true)),
        )
        invalidPayloads.forEachIndexed { index, payload ->
            assertThrows("task payload case $index", IllegalArgumentException::class.java) {
                GoogleCalendarOutboundPreviewSnapshot.fromRemote(
                    task.copy(providerPayload = payload),
                )
            }
        }

        val delete = task.copy(
            operation = GoogleCalendarOutboundOperation.DELETE,
            providerResourceId = "provider-task-id",
            providerEtag = "provider-etag",
            providerPayload = JsonObject(emptyMap()),
        )
        assertEquals(
            GoogleCalendarOutboundOperation.DELETE,
            GoogleCalendarOutboundPreviewSnapshot.fromRemote(delete).operation,
        )
        assertEquals(
            GoogleCalendarOutboundStage.PREVIEWED,
            validIntent().copy(
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                operation = GoogleCalendarOutboundOperation.DELETE,
            ).recordingPreview(delete).stage,
        )
        assertThrows(IllegalArgumentException::class.java) {
            GoogleCalendarOutboundPreviewSnapshot.fromRemote(
                delete.copy(providerPayload = validTaskPayload()),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            GoogleCalendarOutboundPreviewSnapshot.fromRemote(
                delete.copy(providerResourceId = null, providerEtag = null),
            )
        }
    }

    @Test
    fun capabilityCandidateAndTargetConstructorsFailClosed() {
        assertThrows(IllegalArgumentException::class.java) {
            GoogleCalendarOutboundApprovalCapability("dw_ga1_not-a-capability")
        }
        assertThrows(IllegalArgumentException::class.java) {
            GoogleCalendarOutboundCandidate(ZERO_UUID, 1)
        }
        assertThrows(IllegalArgumentException::class.java) {
            GoogleCalendarOutboundCandidate(ITEM_ID, 0)
        }
        assertThrows(IllegalArgumentException::class.java) {
            GoogleCalendarOutboundTarget(ACCOUNT_ID, ZERO_UUID, 1)
        }
        assertThrows(IllegalArgumentException::class.java) {
            GoogleCalendarOutboundTarget(ACCOUNT_ID, COLLECTION_ID, 0)
        }
    }

    @Test
    fun candidateParserAcceptsOnlyCurrentOwnedConfirmedBusyTimedEvent() {
        val item = canonicalEvent()
        val state = syncedState(item)
        assertEquals(
            GoogleCalendarOutboundCandidate(ITEM_ID, 7),
            state.googleCalendarOutboundCandidate(ITEM_ID),
        )

        val pendingOtherItem = PendingCanonicalMutation(
            idempotencyKey = "other-write",
            syncOrigin = "https://api.example.test",
            configurationId = "config-1",
            itemId = OTHER_ITEM_ID,
            expectedRevision = 1,
            targetStatus = "planned",
            startedAt = "2026-09-02T12:00:00Z",
            replacementRequestJson = "{}",
            focusedBlockId = OTHER_ITEM_ID,
            displayStatus = ItemStatus.SCHEDULED,
        )
        val pendingAuthoring = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.TRASH,
            expectedRevision = item.revision,
            createdAt = "2026-09-02T12:00:00Z",
        )
        val rejected = listOf(
            "missing origin" to state.copy(canonicalSyncOrigin = null),
            "missing binding" to state.copy(canonicalConfigurationId = null),
            "missing cursor" to state.copy(canonicalDeltaCursor = null),
            "global canonical uncertainty" to state.copy(
                pendingCanonicalMutation = pendingOtherItem,
            ),
            "same-item authoring" to state.copy(
                pendingCanonicalAuthoringMutations = listOf(pendingAuthoring),
            ),
            "deleted" to syncedState(item.copy(deletedAt = "2026-09-02T12:01:00Z")),
            "zero revision" to syncedState(item.copy(revision = 0)),
            "wrong kind" to syncedState(item.copy(kind = "task")),
            "not planned" to syncedState(item.copy(status = "inbox")),
            "unowned block" to syncedState(
                item.copy(
                    flexibleConstraintsJson = item.flexibleConstraintsJson.replace(
                        "\"owned\":true",
                        "\"owned\":false",
                    ),
                ),
            ),
            "all day" to syncedState(canonicalEvent(allDay = true)),
            "tentative" to syncedState(canonicalEvent(tentative = true)),
            "free" to syncedState(canonicalEvent(busy = false)),
            "invalid bounds" to syncedState(
                canonicalEvent().copy(deadlineAt = "2026-09-02T09:59:00Z"),
            ),
        )
        rejected.forEach { (description, rejectedState) ->
            assertNull(description, rejectedState.googleCalendarOutboundCandidate(ITEM_ID))
        }
    }

    @Test
    fun candidateParserBindsActiveAndRecentTrashEventAndTaskOperations() {
        val activeTask = canonicalTask()
        assertEquals(
            GoogleCalendarOutboundCandidate(
                itemId = ITEM_ID,
                expectedItemRevision = 7,
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                operation = GoogleCalendarOutboundOperation.UPSERT,
            ),
            syncedState(activeTask).googleCalendarOutboundCandidate(ITEM_ID),
        )

        val deletedTask = activeTask.copy(
            revision = 8,
            status = "cancelled",
            recurrenceJson = "{\"type\":\"daily\",\"times_per_day\":1}",
            splitPolicyJson = "{\"type\":\"future_policy\"}",
            deletedAt = "2026-09-02T12:01:00Z",
        )
        val taskTrash = recentlyDeleted(deletedTask)
        assertEquals(
            GoogleCalendarOutboundCandidate(
                itemId = ITEM_ID,
                expectedItemRevision = 8,
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                operation = GoogleCalendarOutboundOperation.DELETE,
            ),
            syncedStateWithTrash(taskTrash).googleCalendarOutboundCandidate(ITEM_ID),
        )

        val deletedEvent = canonicalEvent(allDay = true).copy(
            revision = 8,
            deletedAt = "2026-09-02T12:01:00Z",
        )
        assertEquals(
            GoogleCalendarOutboundCandidate(
                itemId = ITEM_ID,
                expectedItemRevision = 8,
                entityKind = GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
                operation = GoogleCalendarOutboundOperation.DELETE,
            ),
            syncedStateWithTrash(recentlyDeleted(deletedEvent))
                .googleCalendarOutboundCandidate(ITEM_ID),
        )

        val importedTask = activeTask.copy(
            flexibleConstraintsJson = "{\"google_sync\":{}}",
        )
        assertNull(syncedState(importedTask).googleCalendarOutboundCandidate(ITEM_ID))
        assertNull(
            syncedStateWithTrash(
                recentlyDeleted(importedTask.copy(deletedAt = "2026-09-02T12:01:00Z")),
            ).googleCalendarOutboundCandidate(ITEM_ID),
        )
    }

    @Test
    fun targetParserRequiresActiveWriteScopeAndSelectedWritableOwnerOrWriter() {
        val collection = writableCollection()
        assertNotNull(
            googleCalendarOutboundTarget(
                accountId = ACCOUNT_ID,
                accountStatus = "active",
                accountSyncEnabled = true,
                accountHasCalendarWriteScope = true,
                collection = collection,
            ),
        )

        val rejected = listOf(
            "inactive" to TargetCase(collection, accountStatus = "revoked"),
            "disabled" to TargetCase(collection, accountSyncEnabled = false),
            "no write scope" to TargetCase(
                collection,
                accountHasCalendarWriteScope = false,
            ),
            "wrong account" to TargetCase(collection = collection.copy(accountId = OTHER_ITEM_ID)),
            "not selected" to TargetCase(collection = collection.copy(selected = false)),
            "deleted" to TargetCase(collection = collection.copy(providerDeleted = true)),
            "task list" to TargetCase(
                collection = collection.copy(kind = RemoteGoogleCollectionKind.TASK_LIST),
            ),
            "read only" to TargetCase(
                collection = collection.copy(syncRole = RemoteGoogleSyncRole.READ_ONLY),
            ),
            "reader" to TargetCase(collection = collection.copy(providerAccessRole = "reader")),
            "zero revision" to TargetCase(collection = collection.copy(revision = 0)),
        )
        rejected.forEach { (description, case) ->
            assertNull(
                description,
                googleCalendarOutboundTarget(
                    accountId = ACCOUNT_ID,
                    accountStatus = case.accountStatus,
                    accountSyncEnabled = case.accountSyncEnabled,
                    accountHasCalendarWriteScope = case.accountHasCalendarWriteScope,
                    collection = case.collection,
                ),
            )
        }


        val taskList = collection.copy(
            kind = RemoteGoogleCollectionKind.TASK_LIST,
            providerAccessRole = null,
        )
        assertEquals(
            GoogleCalendarOutboundTarget(
                accountId = ACCOUNT_ID,
                collectionId = COLLECTION_ID,
                collectionRevision = 4,
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                operation = GoogleCalendarOutboundOperation.DELETE,
            ),
            googleCalendarOutboundTarget(
                accountId = ACCOUNT_ID,
                accountStatus = "active",
                accountSyncEnabled = true,
                accountHasCalendarWriteScope = false,
                collection = taskList,
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                operation = GoogleCalendarOutboundOperation.DELETE,
                accountHasTasksWriteScope = true,
            ),
        )
        assertNull(
            googleCalendarOutboundTarget(
                accountId = ACCOUNT_ID,
                accountStatus = "active",
                accountSyncEnabled = true,
                accountHasCalendarWriteScope = true,
                collection = taskList,
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                accountHasTasksWriteScope = false,
            ),
        )
        assertNull(
            googleCalendarOutboundTarget(
                accountId = ACCOUNT_ID,
                accountStatus = "active",
                accountSyncEnabled = true,
                accountHasCalendarWriteScope = true,
                collection = collection,
                entityKind = GoogleCalendarOutboundEntityKind.TASK,
                accountHasTasksWriteScope = true,
            ),
        )
    }

    private fun validIntent() = GoogleCalendarOutboundJournal(
        recoveryId = RECOVERY_ID,
        operationGeneration = 1,
        configurationId = "config-1",
        apiBaseUrl = "https://api.example.test/",
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        itemId = ITEM_ID,
        expectedItemRevision = 7,
        entityKind = GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
        intentExpiresAt = "2026-09-02T12:30:00Z",
        createdAt = "2026-09-02T12:00:00Z",
    )

    private fun validRemotePreview() = RemoteGoogleOutboundPreview(
        id = PREVIEW_ID,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        collectionRevision = 4,
        collectionDisplayName = "Private calendar",
        itemId = ITEM_ID,
        itemRevision = 7,
        entityKind = GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
        operation = GoogleCalendarOutboundOperation.UPSERT,
        providerResourceId = null,
        providerEtag = null,
        previewHash = PREVIEW_HASH,
        providerPayload = validPayload(),
        expiresAt = "2026-09-02T12:20:00Z",
    )

    private fun validRemoteTaskPreview() = RemoteGoogleOutboundPreview(
        id = PREVIEW_ID,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        collectionRevision = 4,
        collectionDisplayName = "Personal tasks",
        itemId = ITEM_ID,
        itemRevision = 7,
        entityKind = GoogleCalendarOutboundEntityKind.TASK,
        operation = GoogleCalendarOutboundOperation.UPSERT,
        providerResourceId = null,
        providerEtag = null,
        previewHash = PREVIEW_HASH,
        providerPayload = validTaskPayload(),
        expiresAt = "2026-09-02T12:20:00Z",
    )

    private fun validPayload(): JsonObject = Json.parseToJsonElement(
        """
        {
          "id":"$PROVIDER_EVENT_ID",
          "etag":null,
          "summary":"Private focus",
          "description":"Private notes",
          "location":null,
          "status":"confirmed",
          "transparency":"opaque",
          "visibility":"private",
          "eventType":"default",
          "start":{"date":null,"dateTime":"2026-09-02T10:00:00+02:00","timeZone":"Europe/Paris"},
          "end":{"date":null,"dateTime":"2026-09-02T11:00:00+02:00","timeZone":"Europe/Paris"},
          "attendees":[],
          "attachments":[],
          "recurrence":[],
          "conferenceData":null,
          "recurringEventId":null,
          "originalStartTime":null,
          "updated":null,
          "sequence":null,
          "extendedProperties":{
            "private":{"dayweaveOwnershipProof":"$OWNERSHIP_PROOF"},
            "shared":{}
          }
        }
        """.trimIndent(),
    ) as JsonObject

    private fun validTaskPayload(): JsonObject = Json.parseToJsonElement(
        """
        {
          "id":"",
          "etag":null,
          "title":"Private task",
          "notes":"Private notes",
          "status":"completed",
          "due":"2026-09-03T00:00:00.000Z",
          "completed":"2026-09-02T12:00:00.000Z",
          "updated":null,
          "parent":null,
          "position":null,
          "links":null,
          "deleted":false,
          "hidden":false
        }
        """.trimIndent(),
    ) as JsonObject

    private fun calendarBoundary(dateTime: String) = JsonObject(
        mapOf(
            "date" to JsonNull,
            "dateTime" to JsonPrimitive(dateTime),
            "timeZone" to JsonPrimitive("Europe/Paris"),
        ),
    )

    private fun allDayCalendarBoundary(date: String) = JsonObject(
        mapOf(
            "date" to JsonPrimitive(date),
            "dateTime" to JsonNull,
            "timeZone" to JsonPrimitive("Europe/Paris"),
        ),
    )

    private fun JsonObject.replacing(key: String, value: kotlinx.serialization.json.JsonElement) =
        JsonObject(this + (key to value))

    private fun canonicalEvent(
        allDay: Boolean = false,
        tentative: Boolean = false,
        busy: Boolean = true,
    ): CanonicalItemSnapshot {
        val startsAt = if (allDay) "2026-09-01T22:00:00Z" else "2026-09-02T10:00:00Z"
        val endsAt = if (allDay) "2026-09-02T22:00:00Z" else "2026-09-02T11:00:00Z"
        val timing = CanonicalEventTimingDraft(
            startsAt = startsAt,
            endsAt = endsAt,
            allDay = allDay,
            tentative = tentative,
            busy = busy,
        )
        val draft = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            kind = ItemKind.EVENT,
            title = "Private focus",
            timezoneName = "Europe/Paris",
            durationSeconds = if (allDay) 24 * 60 * 60 else 3_600,
            earliestStartAt = timing.startsAt,
            deadlineAt = timing.endsAt,
            eventTiming = timing,
        )
        return CanonicalItemSnapshot(
            id = ITEM_ID,
            kind = "event",
            status = "planned",
            title = draft.title,
            timezoneName = draft.timezoneName,
            durationSeconds = draft.durationSeconds,
            deadlineAt = draft.deadlineAt,
            earliestStartAt = draft.earliestStartAt,
            flexibleConstraintsJson = draft.constraints.toCanonicalJson(
                timing,
                draft.durationSeconds,
                draft.timezoneName,
            ).toString(),
            splitPolicyJson = draft.split.toCanonicalJson(draft.durationSeconds).toString(),
            importance = draft.importance,
            urgency = draft.urgency,
            siblingOrder = draft.siblingOrder,
            isExecutable = true,
            revision = 7,
            createdAt = "2026-09-02T09:00:00Z",
            updatedAt = "2026-09-02T09:00:00Z",
        )
    }

    private fun canonicalTask(): CanonicalItemSnapshot {
        val draft = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            kind = ItemKind.TASK,
            title = "Private task",
            notes = "Private notes",
            timezoneName = "Europe/Paris",
            durationSeconds = 3_600,
            deadlineAt = "2026-09-03T10:00:00Z",
        )
        return CanonicalItemSnapshot(
            id = ITEM_ID,
            kind = "task",
            status = "planned",
            title = draft.title,
            notes = draft.notes,
            timezoneName = draft.timezoneName,
            durationSeconds = draft.durationSeconds,
            deadlineAt = draft.deadlineAt,
            flexibleConstraintsJson = draft.constraints.toCanonicalJson(
                eventTiming = null,
                durationSeconds = draft.durationSeconds,
            ).toString(),
            splitPolicyJson = draft.split.toCanonicalJson(draft.durationSeconds).toString(),
            importance = draft.importance,
            urgency = draft.urgency,
            siblingOrder = draft.siblingOrder,
            isExecutable = true,
            revision = 7,
            createdAt = "2026-09-02T09:00:00Z",
            updatedAt = "2026-09-02T09:00:00Z",
        )
    }

    private fun recentlyDeleted(item: CanonicalItemSnapshot) = CanonicalRecentlyDeletedRecord(
        id = item.id,
        revision = item.revision,
        deletedAt = requireNotNull(item.deletedAt),
        parentId = item.parentId,
        lastKnownItem = item,
        effectiveIsSensitive = item.isSensitive,
        retentionAnchorAt = item.deletedAt,
    )

    private fun syncedState(item: CanonicalItemSnapshot) = DayWeaveUiState(
        canonicalItems = listOf(item),
        canonicalSyncOrigin = "https://api.example.test",
        canonicalConfigurationId = "config-1",
        canonicalDeltaCursor = "cursor-1",
    )

    private fun syncedStateWithTrash(record: CanonicalRecentlyDeletedRecord) = DayWeaveUiState(
        canonicalRecentlyDeleted = listOf(record),
        canonicalSyncOrigin = "https://api.example.test",
        canonicalConfigurationId = "config-1",
        canonicalDeltaCursor = "cursor-1",
    )

    private fun writableCollection() = RemoteGoogleSyncCollection(
        id = COLLECTION_ID,
        accountId = ACCOUNT_ID,
        kind = RemoteGoogleCollectionKind.CALENDAR,
        remoteCollectionId = "provider-calendar",
        displayName = "Private calendar",
        providerAccessRole = "owner",
        providerPrimary = false,
        providerSelected = true,
        providerHidden = false,
        providerDeleted = false,
        selected = true,
        visible = true,
        syncRole = RemoteGoogleSyncRole.WRITABLE,
        calendarPolicy = RemoteGoogleCalendarPolicy.inboundDefault(),
        revision = 4,
        discoveredAt = "2026-09-02T09:00:00Z",
        configuredAt = "2026-09-02T09:01:00Z",
        lastImportAt = "2026-09-02T09:02:00Z",
        planningProjectionState = RemoteGoogleCalendarProjectionState.COMPLETE,
        planningGeneration = 1,
        planningCollectionRevision = 4,
        planningWindowStart = "2026-09-01T00:00:00Z",
        planningWindowEnd = "2026-09-10T00:00:00Z",
        planningWindowRefreshedAt = "2026-09-02T09:02:00Z",
        createdAt = "2026-09-02T09:00:00Z",
        updatedAt = "2026-09-02T09:02:00Z",
    )

    private data class TargetCase(
        val collection: RemoteGoogleSyncCollection,
        val accountStatus: String = "active",
        val accountSyncEnabled: Boolean = true,
        val accountHasCalendarWriteScope: Boolean = true,
    )

    private companion object {
        const val ZERO_UUID = "00000000-0000-0000-0000-000000000000"
        const val RECOVERY_ID = "11111111-1111-4111-8111-111111111111"
        const val ACCOUNT_ID = "22222222-2222-4222-8222-222222222222"
        const val COLLECTION_ID = "33333333-3333-4333-8333-333333333333"
        const val ITEM_ID = "44444444-4444-4444-8444-444444444444"
        const val OTHER_ITEM_ID = "55555555-5555-4555-8555-555555555555"
        const val PREVIEW_ID = "66666666-6666-4666-8666-666666666666"
        const val MUTATION_ID = "77777777-7777-4777-8777-777777777777"
        const val PREVIEW_HASH =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        const val CAPABILITY = "dw_ga1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        val PROVIDER_EVENT_ID = "d1" + "a".repeat(64)
        const val OWNERSHIP_PROOF = "[server-managed]"
    }
}
