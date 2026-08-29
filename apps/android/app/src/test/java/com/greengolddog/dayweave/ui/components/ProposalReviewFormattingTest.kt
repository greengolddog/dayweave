package com.greengolddog.dayweave.ui.components

import com.greengolddog.dayweave.network.RemoteProposalCanonicalItem
import com.greengolddog.dayweave.network.RemoteProposalItemField
import com.greengolddog.dayweave.network.RemoteProposalItemKind
import com.greengolddog.dayweave.network.RemoteProposalItemStatus
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Test

class ProposalReviewFormattingTest {
    @Test
    fun everyMaterialCanonicalFieldHasAnExactBeforeAfterRepresentation() {
        val item = item()
        val expected = linkedMapOf(
            RemoteProposalItemField.IS_SENSITIVE to "true",
            RemoteProposalItemField.KIND to "\"habit\"",
            RemoteProposalItemField.STATUS to "\"paused\"",
            RemoteProposalItemField.TITLE to "\"Focus \\\"deeply\\\"\"",
            RemoteProposalItemField.NOTES to "\"Line one\\nLine two\"",
            RemoteProposalItemField.TIMEZONE_NAME to "\"Europe/Madrid\"",
            RemoteProposalItemField.DURATION_SECONDS to "2700",
            RemoteProposalItemField.DEADLINE_AT to "\"2026-09-01T17:00:00Z\"",
            RemoteProposalItemField.EARLIEST_START_AT to "\"2026-09-01T08:00:00Z\"",
            RemoteProposalItemField.RECURRENCE to "{\"frequency\":\"weekly\"}",
            RemoteProposalItemField.FLEXIBLE_CONSTRAINTS to "{\"window\":\"morning\"}",
            RemoteProposalItemField.SPLIT_POLICY to "{\"type\":\"indivisible\"}",
            RemoteProposalItemField.IMPORTANCE to "73",
            RemoteProposalItemField.URGENCY to "61",
            RemoteProposalItemField.PARENT_ID to "\"22222222-2222-4222-8222-222222222222\"",
            RemoteProposalItemField.SIBLING_ORDER to "4",
            RemoteProposalItemField.IS_EXECUTABLE to "false",
            RemoteProposalItemField.REVISION to "7",
            RemoteProposalItemField.COMPLETED_AT to "\"2026-09-01T18:00:00Z\"",
            RemoteProposalItemField.DELETED_AT to "\"2026-09-02T18:00:00Z\"",
        )

        assertEquals(RemoteProposalItemField.entries, expected.keys.toList())
        expected.forEach { (field, value) ->
            assertEquals(value, proposalReviewFieldValue(item, field, concealSensitive = false))
        }
        assertEquals(
            "Not present",
            proposalReviewFieldValue(null, RemoteProposalItemField.TITLE, concealSensitive = true),
        )
    }

    @Test
    fun sensitiveReviewConcealsEveryValueExceptTheSensitivityFlagUntilReveal() {
        val item = item()

        RemoteProposalItemField.entries.forEach { field ->
            val expected = if (field == RemoteProposalItemField.IS_SENSITIVE) {
                "true"
            } else {
                "Concealed"
            }
            assertEquals(expected, proposalReviewFieldValue(item, field, concealSensitive = true))
        }
        assertEquals(
            "\"Focus \\\"deeply\\\"\"",
            proposalReviewFieldValue(
                item,
                RemoteProposalItemField.TITLE,
                concealSensitive = false,
            ),
        )
        assertEquals(
            "id=Concealed · " +
                "title=Concealed · kind=Concealed · status=Concealed",
            proposalReviewIdentitySnapshot(item, concealSensitive = true),
        )
        assertEquals(
            "id=11111111-1111-4111-8111-111111111111 · " +
                "title=\"Focus \\\"deeply\\\"\" · kind=\"habit\" · status=\"paused\"",
            proposalReviewIdentitySnapshot(item, concealSensitive = false),
        )
        assertEquals("Not present", proposalReviewIdentitySnapshot(null, concealSensitive = true))
    }

    private fun item() = RemoteProposalCanonicalItem(
        id = "11111111-1111-4111-8111-111111111111",
        isSensitive = true,
        kind = RemoteProposalItemKind.HABIT,
        status = RemoteProposalItemStatus.PAUSED,
        title = "Focus \"deeply\"",
        notes = "Line one\nLine two",
        timezoneName = "Europe/Madrid",
        durationSeconds = 2_700,
        deadlineAt = "2026-09-01T17:00:00Z",
        earliestStartAt = "2026-09-01T08:00:00Z",
        recurrence = buildJsonObject { put("frequency", "weekly") },
        flexibleConstraints = buildJsonObject { put("window", "morning") },
        splitPolicy = buildJsonObject { put("type", "indivisible") },
        importance = 73,
        urgency = 61,
        parentId = "22222222-2222-4222-8222-222222222222",
        siblingOrder = 4,
        isExecutable = false,
        revision = 7,
        createdAt = "2026-08-30T10:00:00Z",
        updatedAt = "2026-09-02T18:00:00Z",
        completedAt = "2026-09-01T18:00:00Z",
        deletedAt = "2026-09-02T18:00:00Z",
    )
}
