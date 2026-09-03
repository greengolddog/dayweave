package com.greengolddog.dayweave.model

import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationAccepted
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationApproval
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationChange
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationPreview
import com.greengolddog.dayweave.network.RemoteScheduleGooglePublicationStatus
import com.greengolddog.dayweave.network.ScheduleGooglePublicationOperation
import com.greengolddog.dayweave.network.ScheduleGooglePublicationState
import java.util.Base64
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleSchedulePublicationModelsTest {
    @Test
    fun journalAdvancesOneWayAndStripsCapabilityAtAcceptance() {
        val intent = validIntent()
        val previewed = intent.recordingPreview(validPreview())
        val attempted = previewed.recordingApprovalAttempt()
        val approved = attempted.recordingApproval(validApproval())
        val accepted = approved.recordingAcceptance(
            RemoteScheduleGooglePublicationAccepted(PUBLICATION_ID, replayed = false),
        )
        val completed = accepted.recordingStatus(validStatus())

        assertTrue(intent.canTransitionTo(previewed))
        assertTrue(previewed.canTransitionTo(attempted))
        assertTrue(attempted.canTransitionTo(approved))
        assertTrue(approved.canTransitionTo(accepted))
        assertTrue(accepted.canTransitionTo(completed))
        assertNull(accepted.approvalCapability)
        assertNull(accepted.approvalExpiresAt)
        assertTrue(completed.status?.isTerminal == true)
        assertFalse(completed.toString().contains(CAPABILITY))
        assertFalse(approved.approvalCapability.toString().contains(CAPABILITY))
    }

    @Test
    fun previewRejectsCountOrdinalAndProviderBindingTampering() {
        val valid = GoogleSchedulePublicationPreviewSnapshot.fromRemote(validPreview())
        assertEquals(1, valid.changes.size)

        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(createCount = 0, noopCount = 1)
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(changes = listOf(valid.changes.single().copy(ordinal = 1)))
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(
                changes = listOf(
                    valid.changes.single().copy(providerResourceId = "unexpected"),
                ),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(changes = listOf(valid.changes.single().copy(sourceBlockId = null)))
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(
                createCount = 2,
                changes = listOf(valid.changes.single(), valid.changes.single().copy(ordinal = 1)),
            )
        }
    }

    @Test
    fun statusRejectsAggregatePriorityAndChronologyTampering() {
        val valid = GoogleSchedulePublicationStatusSnapshot.fromRemote(validStatus())
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(
                state = ScheduleGooglePublicationState.DELIVERING,
                deliveringCount = 0,
                publishedCount = 0,
                pendingCount = 1,
                completedAt = null,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(
                state = ScheduleGooglePublicationState.PARTIALLY_PUBLISHED,
                totalCount = 2,
                publishedCount = 1,
                pendingCount = 1,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(
                state = ScheduleGooglePublicationState.CONFLICT,
                publishedCount = 1,
                conflictedCount = 0,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(completedAt = "2026-09-03T12:09:59Z")
        }
        assertThrows(IllegalArgumentException::class.java) {
            valid.copy(
                state = ScheduleGooglePublicationState.SUPERSEDED,
                totalCount = 0,
                publishedCount = 0,
                supersededCount = 0,
            )
        }
    }

    @Test
    fun publicDiagnosticsContainNeitherReviewContentNorIdentifiers() {
        val preview = GoogleSchedulePublicationPreviewSnapshot.fromRemote(validPreview())
        val status = GoogleSchedulePublicationStatusSnapshot.fromRemote(validStatus())
        for (diagnostic in listOf(preview.toString(), preview.changes.single().toString(), status.toString())) {
            assertFalse(diagnostic.contains("Focus block"))
            assertFalse(diagnostic.contains(PREVIEW_ID))
            assertFalse(diagnostic.contains(ACCOUNT_ID))
            assertFalse(diagnostic.contains(COLLECTION_ID))
        }
    }

    private fun validIntent() = GoogleSchedulePublicationJournal(
        recoveryId = RECOVERY_ID,
        operationGeneration = 1,
        configurationId = "test-binding",
        apiBaseUrl = "https://api.example.test/",
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        expectedScheduleRevisionId = SCHEDULE_REVISION_ID,
        intentExpiresAt = "2026-09-03T12:30:00Z",
        createdAt = "2026-09-03T12:00:00Z",
    )

    private fun validPreview() = RemoteScheduleGooglePublicationPreview(
        id = PREVIEW_ID,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        collectionRevision = 7,
        collectionDisplayName = "Planning",
        scheduleRevisionId = SCHEDULE_REVISION_ID,
        scheduleRevisionNumber = 11,
        previewHash = "a".repeat(64),
        createCount = 1,
        updateCount = 0,
        deleteCount = 0,
        noopCount = 0,
        changes = listOf(
            RemoteScheduleGooglePublicationChange(
                ordinal = 0,
                slotId = SLOT_ID,
                sourceBlockId = SOURCE_BLOCK_ID,
                operation = ScheduleGooglePublicationOperation.CREATE,
                providerResourceId = null,
                providerEtag = null,
                summary = "Focus block",
                startsAt = "2026-09-03T13:00:00Z",
                endsAt = "2026-09-03T14:00:00Z",
            ),
        ),
        expiresAt = "2026-09-03T12:20:00Z",
    )

    private fun validApproval() = RemoteScheduleGooglePublicationApproval(
        PREVIEW_ID,
        CAPABILITY,
        "2026-09-03T12:15:00Z",
    )

    private fun validStatus() = RemoteScheduleGooglePublicationStatus(
        publicationId = PUBLICATION_ID,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        scheduleRevisionId = SCHEDULE_REVISION_ID,
        state = ScheduleGooglePublicationState.PUBLISHED,
        totalCount = 1,
        pendingCount = 0,
        deliveringCount = 0,
        publishedCount = 1,
        conflictedCount = 0,
        failedCount = 0,
        supersededCount = 0,
        createdAt = "2026-09-03T12:10:00Z",
        completedAt = "2026-09-03T12:11:00Z",
        lastErrorCode = null,
    )

    private companion object {
        const val RECOVERY_ID = "11111111-1111-4111-8111-111111111111"
        const val ACCOUNT_ID = "22222222-2222-4222-8222-222222222222"
        const val COLLECTION_ID = "33333333-3333-4333-8333-333333333333"
        const val SCHEDULE_REVISION_ID = "44444444-4444-4444-8444-444444444444"
        const val PREVIEW_ID = "55555555-5555-4555-8555-555555555555"
        const val SLOT_ID = "66666666-6666-4666-8666-666666666666"
        const val SOURCE_BLOCK_ID = "77777777-7777-4777-8777-777777777777"
        const val PUBLICATION_ID = "88888888-8888-4888-8888-888888888888"
        val CAPABILITY = "dw_gsa1_" + Base64.getUrlEncoder().withoutPadding()
            .encodeToString(ByteArray(32) { (it + 1).toByte() })
    }
}
