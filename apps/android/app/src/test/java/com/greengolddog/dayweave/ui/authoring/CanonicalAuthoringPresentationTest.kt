package com.greengolddog.dayweave.ui.authoring

import com.greengolddog.dayweave.model.CanonicalAuthoringDisposition
import com.greengolddog.dayweave.model.CanonicalAuthoringOperation
import com.greengolddog.dayweave.model.CanonicalBlockedReasonKind
import com.greengolddog.dayweave.model.CanonicalConstraintStrengthDraft
import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDeadlineStrength
import com.greengolddog.dayweave.model.CanonicalDependencyDraft
import com.greengolddog.dayweave.model.CanonicalDependencyRelation
import com.greengolddog.dayweave.model.CanonicalDraftPlacement
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.model.CanonicalItemDraft
import com.greengolddog.dayweave.model.CanonicalItemSnapshot
import com.greengolddog.dayweave.model.CanonicalFlexibleConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalSchedulingConstraintsDraft
import com.greengolddog.dayweave.model.CanonicalRecentlyDeletedRecord
import com.greengolddog.dayweave.model.DayWeaveUiState
import com.greengolddog.dayweave.model.PendingCanonicalAuthoringMutation
import com.greengolddog.dayweave.model.toCanonicalDraft
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalAuthoringPresentationTest {
    @Test
    fun buildsInboxPlannedConflictAndRecentlyDeletedSectionsWithHierarchy() {
        val parent = item(PARENT_ID, "Project", "inbox")
        val child = item(CHILD_ID, "Next action", "planned", parentId = PARENT_ID)
        val conflict = PendingCanonicalAuthoringMutation(
            id = CONFLICT_MUTATION_ID,
            itemId = CONFLICT_ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = CanonicalItemDraft(
                title = "Conflicted capture",
                timezoneName = "UTC",
            ),
            createdAt = NOW,
            syncOrigin = "https://example.test/",
            submittedAt = "2026-08-30T10:00:01Z",
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "Canonical identity already exists",
        )
        val deletedItem = item(DELETED_ID, "Recover me", "inbox").copy(
            revision = 2,
            updatedAt = "2026-08-30T10:02:00Z",
            deletedAt = "2026-08-30T10:02:00Z",
        )
        val deleted = CanonicalRecentlyDeletedRecord(
            id = deletedItem.id,
            revision = deletedItem.revision,
            deletedAt = requireNotNull(deletedItem.deletedAt),
            lastKnownItem = deletedItem,
            effectiveIsSensitive = false,
            retentionAnchorAt = requireNotNull(deletedItem.deletedAt),
        )

        val presentation = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(
                canonicalItems = listOf(parent, child),
                pendingCanonicalAuthoringMutations = listOf(conflict),
                canonicalRecentlyDeleted = listOf(deleted),
            ),
        )

        assertEquals(setOf("Project", "Conflicted capture"), presentation.inbox.map { it.title }.toSet())
        assertEquals(listOf("Next action"), presentation.planned.map { it.title })
        assertEquals(listOf("Conflicted capture"), presentation.conflicts.map { it.title })
        assertEquals(listOf("Recover me"), presentation.recentlyDeleted.map { it.title })
        val childRow = presentation.planned.single()
        assertEquals(1, childRow.depth)
        assertEquals(listOf("Project"), childRow.breadcrumb)
        assertEquals(CanonicalAuthoringSyncState.CONFLICTED, presentation.conflicts.single().syncState)
    }

    @Test
    fun unsentReplacementOverlaysCanonicalBodyAndRemainsEditable() {
        val canonical = item(PARENT_ID, "Server title", "inbox")
        val replacement = PendingCanonicalAuthoringMutation(
            id = REPLACE_MUTATION_ID,
            itemId = canonical.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = canonical.toCanonicalDraft().copy(
                title = "Queued title",
                placement = CanonicalDraftPlacement.PLANNED,
            ),
            expectedRevision = canonical.revision,
            baseItem = canonical,
            createdAt = NOW,
        )

        val presentation = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(
                canonicalItems = listOf(canonical),
                pendingCanonicalAuthoringMutations = listOf(replacement),
            ),
        )

        assertTrue(presentation.inbox.isEmpty())
        val row = presentation.planned.single()
        assertEquals("Queued title", row.title)
        assertEquals(CanonicalAuthoringRowSource.PENDING_REPLACE, row.source)
        assertEquals(CanonicalAuthoringSyncState.QUEUED, row.syncState)
        assertFalse(row.isReadOnly)
        assertFalse(row.canTrash)
        assertEquals(REPLACE_MUTATION_ID, row.mutationId)
    }

    @Test
    fun pendingCreateAndReplacePresentTheirRangedAssistantDurationExactly() {
        val rangedDraft = CanonicalItemDraft(
            placement = CanonicalDraftPlacement.PLANNED,
            title = "Ranged draft",
            timezoneName = "UTC",
            durationSeconds = 3_600,
            durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 1_800,
            durationMaxSeconds = 5_400,
            durationSource = CanonicalDurationSource.ASSISTANT,
        )
        val create = PendingCanonicalAuthoringMutation(
            id = CONFLICT_MUTATION_ID,
            itemId = CONFLICT_ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = rangedDraft,
            createdAt = NOW,
        )
        val createRow = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(pendingCanonicalAuthoringMutations = listOf(create)),
        ).planned.single()
        assertEquals(CanonicalDurationKind.RANGE, createRow.durationKind)
        assertEquals(CanonicalDurationSource.ASSISTANT, createRow.durationSource)
        assertEquals("30m–90m · expected 1h · Assistant", canonicalDurationLabel(createRow))

        val canonical = item(PARENT_ID, "Server exact", "planned")
        val replace = PendingCanonicalAuthoringMutation(
            id = REPLACE_MUTATION_ID,
            itemId = canonical.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = rangedDraft.copy(title = "Ranged replacement"),
            expectedRevision = canonical.revision,
            baseItem = canonical,
            createdAt = NOW,
        )
        val replaceRow = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(
                canonicalItems = listOf(canonical),
                pendingCanonicalAuthoringMutations = listOf(replace),
            ),
        ).planned.single()
        assertEquals(CanonicalDurationKind.RANGE, replaceRow.durationKind)
        assertEquals(CanonicalDurationSource.ASSISTANT, replaceRow.durationSource)
        assertEquals("30m–90m · expected 1h · Assistant", canonicalDurationLabel(replaceRow))
    }

    @Test
    fun graphAuthorityMetadataRemainsVisibleWithClearReadOnlyDiagnostic() {
        val item = item(PARENT_ID, "Linked task", "inbox").copy(
            flexibleConstraintsJson =
                """{"goal_ids":["$CHILD_ID"]}""",
        )

        val presentation = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = listOf(item)),
        )

        val row = presentation.inbox.single()
        assertEquals("Linked task", row.title)
        assertTrue(row.isReadOnly)
        assertTrue(row.canTrash)
        assertTrue(row.diagnostic.orEmpty().contains("read-only"))
    }

    @Test
    fun blockedAndTypedTimingMetadataRemainVisibleWithoutLegacyReinterpretation() {
        val blocker = item(PARENT_ID, "External prerequisite", "inbox")
        val blocked = item(CHILD_ID, "Waiting action", "blocked").copy(
            durationSeconds = 3_600,
            durationKind = CanonicalDurationKind.RANGE,
            durationMinSeconds = 1_800,
            durationMaxSeconds = 5_400,
            durationSource = CanonicalDurationSource.ASSISTANT,
            deadlineAt = null,
            deadlineKind = CanonicalDeadlineKind.DATE,
            deadlineDate = "2026-09-30",
            deadlineStrength = CanonicalDeadlineStrength.SOFT,
            deadlineSoftWeight = 42,
            blockedReasonKind = CanonicalBlockedReasonKind.DEPENDENCY,
            blockedByItemId = blocker.id,
            blockedReason = "Vendor approval",
            hasExplicitStructuralMetadata = true,
        )

        val presentation = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = listOf(blocker, blocked)),
        )

        val row = presentation.blocked.single()
        assertEquals("Waiting action", row.title)
        assertEquals("blocked", row.status)
        assertTrue(row.isReadOnly)
        assertEquals(2, presentation.itemCount)
        assertEquals(
            "30m–90m · expected 1h · Assistant",
            canonicalDurationLabel(row),
        )
        assertEquals("Due 2026-09-30 · Soft · weight 42", canonicalTimingLabel(row))
        assertEquals(
            "Blocked · waiting for External prerequisite · Vendor approval",
            canonicalBlockedReasonLabel(row),
        )
        assertEquals(listOf(PARENT_ID), row.blockingDependencies.map { it.itemId })

        val event = item(CONFLICT_ITEM_ID, "Calendar interval", "planned").copy(
            kind = "event",
            deadlineAt = "2026-09-03T11:00:00Z",
            deadlineKind = CanonicalDeadlineKind.NONE,
            deadlineDate = null,
            deadlineStrength = null,
        )
        val eventRow = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = listOf(event)),
        ).planned.single()
        assertTrue(canonicalTimingLabel(eventRow).orEmpty().startsWith("Ends "))
        assertFalse(canonicalTimingLabel(eventRow).orEmpty().startsWith("Due "))
    }

    @Test
    fun derivesEveryUnmetHardBlockerAndRedactsSensitiveCrossItemContent() {
        val ordinary = item(PARENT_ID, "Publish draft", "planned")
        val sensitive = item(SENSITIVE_ID, "Private medical approval", "planned").copy(
            isSensitive = true,
        )
        val completed = item(COMPLETED_ID, "Finished prerequisite", "completed")
        val soft = item(SOFT_ID, "Preferred warm-up", "planned")
        val dependencies = listOf(
            CanonicalDependencyDraft(
                itemId = ordinary.id,
                relation = CanonicalDependencyRelation.FINISH_TO_START,
                strength = CanonicalConstraintStrengthDraft.hard(),
            ),
            CanonicalDependencyDraft(
                itemId = sensitive.id,
                relation = CanonicalDependencyRelation.START_TO_START,
                minimumLagMinutes = 15,
                strength = CanonicalConstraintStrengthDraft.hard(),
            ),
            CanonicalDependencyDraft(
                itemId = completed.id,
                relation = CanonicalDependencyRelation.FINISH_TO_FINISH,
                strength = CanonicalConstraintStrengthDraft.hard(),
            ),
            CanonicalDependencyDraft(
                itemId = soft.id,
                relation = CanonicalDependencyRelation.START_TO_FINISH,
                strength = CanonicalConstraintStrengthDraft.soft(80),
            ),
        )
        val blocked = item(CHILD_ID, "Waiting action", "blocked").copy(
            flexibleConstraintsJson = CanonicalFlexibleConstraintsDraft(
                scheduling = CanonicalSchedulingConstraintsDraft(dependencies = dependencies),
            ).toCanonicalJson(null).toString(),
            blockedReasonKind = CanonicalBlockedReasonKind.DEPENDENCY,
            blockedByItemId = sensitive.id,
            blockedReason = "Private medical approval is outstanding",
            hasExplicitStructuralMetadata = true,
        )

        val row = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(
                canonicalItems = listOf(ordinary, sensitive, completed, soft, blocked),
            ),
        ).blocked.single()

        assertEquals(4, row.dependencies.size)
        assertEquals(
            listOf(PARENT_ID, SENSITIVE_ID),
            row.blockingDependencies.map(CanonicalDependencyPresentation::itemId),
        )
        val sensitiveCause = row.blockingDependencies.single { it.itemId == SENSITIVE_ID }
        assertTrue(sensitiveCause.isSensitive)
        assertFalse(sensitiveCause.displayTitle.contains("medical", ignoreCase = true))
        assertTrue(sensitiveCause.displayTitle.startsWith("Sensitive item"))
        assertEquals(
            "Start → start · lag 15m · Hard · Planned",
            canonicalDependencyDetail(sensitiveCause),
        )
        val summary = requireNotNull(canonicalBlockedReasonLabel(row))
        assertEquals("Blocked · waiting for 2 predecessors", summary)
        assertFalse(summary.contains("medical", ignoreCase = true))
    }

    @Test
    fun opaqueDependencyMetadataRemainsVisibleAndCompatibilityBlockerStaysRedacted() {
        val sensitive = item(SENSITIVE_ID, "Private medical approval", "planned").copy(
            isSensitive = true,
        )
        val blocked = item(CHILD_ID, "Waiting action", "blocked").copy(
            flexibleConstraintsJson =
                """{"constraints":{"dependencies":{"schema_version":2}}}""",
            blockedReasonKind = CanonicalBlockedReasonKind.DEPENDENCY,
            blockedByItemId = sensitive.id,
            blockedReason = "Private medical approval is outstanding",
            hasExplicitStructuralMetadata = true,
        )

        val row = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = listOf(sensitive, blocked)),
        ).blocked.single()

        assertTrue(row.hasOpaqueDependencies)
        val compatibilityBlocker = row.blockingDependencies.single()
        assertTrue(compatibilityBlocker.isSensitive)
        assertFalse(compatibilityBlocker.displayTitle.contains("medical", ignoreCase = true))
        val summary = requireNotNull(canonicalBlockedReasonLabel(row))
        assertFalse(summary.contains("medical", ignoreCase = true))
    }

    @Test
    fun providerEventMetadataStaysVisibleAndReadOnly() {
        val providerEvent = item(PARENT_ID, "Imported meeting", "inbox").copy(
            kind = "event",
            flexibleConstraintsJson = """
                {
                  "calendar_event": {
                    "start": "2026-09-03T10:00:00Z",
                    "end": "2026-09-03T11:00:00Z",
                    "immutable": true,
                    "all_day": false,
                    "source_calendar_id": "primary"
                  }
                }
            """.trimIndent(),
        )

        val row = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = listOf(providerEvent)),
        ).inbox.single()

        assertEquals("Imported meeting", row.title)
        assertTrue(row.isReadOnly)
        assertTrue(row.canTrash)
        assertTrue(row.diagnostic.orEmpty().contains("Provider-managed"))
    }

    @Test
    fun partialInboxEventFailsClosedButCanStillBeTrashed() {
        val partial = item(PARENT_ID, "Unresolved meeting", "inbox").copy(
            kind = "event",
            durationSeconds = 1_800,
        )

        val row = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = listOf(partial)),
        ).inbox.single()

        assertTrue(row.isReadOnly)
        assertTrue(row.canTrash)
        assertTrue(row.diagnostic.orEmpty().contains("partial fixed timing"))
    }

    @Test
    fun finiteCustomRruleIsVisibleAndEditable() {
        val custom = item(PARENT_ID, "Custom recurrence", "inbox").copy(
            recurrenceJson =
                """{"type":"custom","rrule":"FREQ=MONTHLY;INTERVAL=1;BYMONTHDAY=-1,1;COUNT=24"}""",
        )

        val row = CanonicalAuthoringPresentation.build(
            DayWeaveUiState(canonicalItems = listOf(custom)),
        ).inbox.single()

        assertEquals(
            "FREQ=MONTHLY;INTERVAL=1;BYMONTHDAY=-1,1;COUNT=24",
            row.draft?.recurrence?.rrule,
        )
        assertFalse(row.isReadOnly)
        assertTrue(row.canTrash)
        assertNull(row.diagnostic)
    }

    private fun item(
        id: String,
        title: String,
        status: String,
        parentId: String? = null,
    ) = CanonicalItemSnapshot(
        id = id,
        kind = "task",
        status = status,
        title = title,
        timezoneName = "UTC",
        flexibleConstraintsJson = "{}",
        splitPolicyJson = "{\"type\":\"indivisible\"}",
        importance = 50,
        urgency = 50,
        parentId = parentId,
        siblingOrder = 0,
        isExecutable = status == "planned",
        revision = 1,
        createdAt = NOW,
        updatedAt = NOW,
    )

    private companion object {
        const val PARENT_ID = "11111111-1111-4111-8111-111111111111"
        const val CHILD_ID = "22222222-2222-4222-8222-222222222222"
        const val CONFLICT_ITEM_ID = "33333333-3333-4333-8333-333333333333"
        const val DELETED_ID = "44444444-4444-4444-8444-444444444444"
        const val CONFLICT_MUTATION_ID = "55555555-5555-4555-8555-555555555555"
        const val REPLACE_MUTATION_ID = "66666666-6666-4666-8666-666666666666"
        const val SENSITIVE_ID = "77777777-7777-4777-8777-777777777777"
        const val COMPLETED_ID = "88888888-8888-4888-8888-888888888888"
        const val SOFT_ID = "99999999-9999-4999-8999-999999999999"
        const val NOW = "2026-08-30T10:00:00Z"
    }
}
