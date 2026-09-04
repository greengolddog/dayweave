package com.greengolddog.dayweave.ui.components

import com.greengolddog.dayweave.model.CanonicalDeadlineKind
import com.greengolddog.dayweave.model.CanonicalDeadlineStrength
import com.greengolddog.dayweave.model.CanonicalDurationKind
import com.greengolddog.dayweave.model.CanonicalDurationSource
import com.greengolddog.dayweave.network.RemoteProposalCanonicalItem
import com.greengolddog.dayweave.network.RemoteProposalItemField
import com.greengolddog.dayweave.network.RemoteProposalItemKind
import com.greengolddog.dayweave.network.RemoteProposalItemStatus
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.add
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
            RemoteProposalItemField.DURATION_KIND to "\"exact\"",
            RemoteProposalItemField.DURATION_MIN_SECONDS to "2700",
            RemoteProposalItemField.DURATION_SECONDS to "2700",
            RemoteProposalItemField.DURATION_MAX_SECONDS to "2700",
            RemoteProposalItemField.DURATION_SOURCE to "\"user\"",
            RemoteProposalItemField.DEADLINE_KIND to "\"date_time\"",
            RemoteProposalItemField.DEADLINE_AT to "\"2026-09-01T17:00:00Z\"",
            RemoteProposalItemField.DEADLINE_DATE to "null",
            RemoteProposalItemField.DEADLINE_STRENGTH to "\"hard\"",
            RemoteProposalItemField.DEADLINE_SOFT_WEIGHT to "null",
            RemoteProposalItemField.EARLIEST_START_AT to "\"2026-09-01T08:00:00Z\"",
            RemoteProposalItemField.RECURRENCE to "{\"frequency\":\"weekly\"}",
            RemoteProposalItemField.FLEXIBLE_CONSTRAINTS to "{\"window\":\"morning\"}",
            RemoteProposalItemField.DEPENDENCIES to
                "[{\"item_id\":\"33333333-3333-4333-8333-333333333333\"," +
                "\"relation\":\"finish_to_start\",\"minimum_lag\":30," +
                "\"strength\":{\"level\":\"hard\"}}]",
            RemoteProposalItemField.SPLIT_POLICY to "{\"type\":\"indivisible\"}",
            RemoteProposalItemField.IMPORTANCE to "73",
            RemoteProposalItemField.URGENCY to "61",
            RemoteProposalItemField.PARENT_ID to "\"22222222-2222-4222-8222-222222222222\"",
            RemoteProposalItemField.SIBLING_ORDER to "4",
            RemoteProposalItemField.HAS_OWN_EFFORT to "false",
            RemoteProposalItemField.BLOCKED_REASON_KIND to "null",
            RemoteProposalItemField.BLOCKED_BY_ITEM_ID to "null",
            RemoteProposalItemField.BLOCKED_REASON to "null",
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
        durationKind = CanonicalDurationKind.EXACT,
        durationMinSeconds = 2_700,
        durationMaxSeconds = 2_700,
        durationSource = CanonicalDurationSource.USER,
        deadlineAt = "2026-09-01T17:00:00Z",
        deadlineKind = CanonicalDeadlineKind.DATE_TIME,
        deadlineStrength = CanonicalDeadlineStrength.HARD,
        earliestStartAt = "2026-09-01T08:00:00Z",
        recurrence = buildJsonObject { put("frequency", "weekly") },
        flexibleConstraints = buildJsonObject {
            put("window", "morning")
            put(
                "constraints",
                buildJsonObject {
                    put(
                        "dependencies",
                        buildJsonArray {
                            add(
                                buildJsonObject {
                                    put("item_id", "33333333-3333-4333-8333-333333333333")
                                    put("relation", "finish_to_start")
                                    put("minimum_lag", 30)
                                    put("strength", buildJsonObject { put("level", "hard") })
                                },
                            )
                        },
                    )
                },
            )
        },
        splitPolicy = buildJsonObject { put("type", "indivisible") },
        importance = 73,
        urgency = 61,
        parentId = "22222222-2222-4222-8222-222222222222",
        siblingOrder = 4,
        hasOwnEffort = false,
        isExecutable = false,
        revision = 7,
        createdAt = "2026-08-30T10:00:00Z",
        updatedAt = "2026-09-02T18:00:00Z",
        completedAt = "2026-09-01T18:00:00Z",
        deletedAt = "2026-09-02T18:00:00Z",
    )
}
