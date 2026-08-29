package com.greengolddog.dayweave

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.network.ProposalPreviewMember
import com.greengolddog.dayweave.network.RemoteProposalApplicationPreview
import com.greengolddog.dayweave.network.RemoteProposalCanonicalItem
import com.greengolddog.dayweave.network.RemoteProposalItemDiff
import com.greengolddog.dayweave.network.RemoteProposalItemField
import com.greengolddog.dayweave.network.RemoteProposalItemKind
import com.greengolddog.dayweave.network.RemoteProposalItemStatus
import com.greengolddog.dayweave.network.RemoteProposalImplicitChangeReason
import com.greengolddog.dayweave.network.RemoteProposalImplicitItemDiff
import com.greengolddog.dayweave.network.RemoteProposalOperation
import com.greengolddog.dayweave.network.RemoteProposalRisk
import com.greengolddog.dayweave.network.RemoteProposalRiskCode
import com.greengolddog.dayweave.network.RemoteProposalRiskLevel
import com.greengolddog.dayweave.sync.ProposalApplicationPhase
import com.greengolddog.dayweave.sync.ProposalApplicationApproval
import com.greengolddog.dayweave.sync.ProposalApplicationState
import com.greengolddog.dayweave.ui.components.ProposalReviewDialog
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertTrue
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ProposalApplicationUiTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun exactApplyRequiresSeparateExplicitApproval() {
        val applied = AtomicBoolean(false)
        val approval = AtomicReference<ProposalApplicationApproval?>()
        composeRule.setContent {
            MaterialTheme {
                ProposalReviewDialog(
                    proposalTitle = "Create a focus task",
                    state = ProposalApplicationState(
                        phase = ProposalApplicationPhase.REVIEW_READY,
                        message = "Review complete.",
                        activeProposalId = PROPOSAL_ID,
                        preview = preview(),
                    ),
                    onDismiss = {},
                    onRegenerate = {},
                    onApply = {
                        approval.set(it)
                        applied.set(true)
                    },
                )
            }
        }

        composeRule.onNodeWithText("Direct changes").assertIsDisplayed()
        composeRule.onNodeWithText("High: This changes a deadline · approval required")
            .assertIsDisplayed()
        composeRule.onNodeWithText("After: \"Focus task\"").assertIsDisplayed()
        composeRule.onNodeWithText("Hierarchy side effects").assertIsDisplayed()
        composeRule.onNodeWithText(
            "Identity before: id=$IMPLICIT_ITEM_ID · title=\"Focus parent\" · " +
                "kind=\"task\" · status=\"planned\"",
        ).assertIsDisplayed()
        composeRule.onNodeWithTag("proposal_apply_exact_review").assertIsNotEnabled()
        composeRule.onNodeWithTag("proposal_explicit_approval").performClick()
        composeRule.onNodeWithTag("proposal_apply_exact_review").assertIsEnabled().performClick()
        assertTrue(applied.get())
        assertEquals(PROPOSAL_ID, approval.get()?.proposalId)
        assertEquals(1L, approval.get()?.expectedProposalRevision)
        assertEquals(PREVIEW_ID, approval.get()?.previewId)
        assertEquals("sha256:${"a".repeat(64)}", approval.get()?.reviewHash)
    }

    @Test
    fun sensitiveExactValuesRequireEphemeralReveal() {
        val sensitivePreview = preview().copy(
            diffs = preview().diffs.map { diff ->
                diff.copy(
                    changedFields = listOf(
                        RemoteProposalItemField.IS_SENSITIVE,
                        RemoteProposalItemField.TITLE,
                        RemoteProposalItemField.NOTES,
                    ),
                    after = diff.after?.copy(
                        isSensitive = true,
                        notes = "Private exact note",
                    ),
                )
            },
        )
        composeRule.setContent {
            MaterialTheme {
                ProposalReviewDialog(
                    proposalTitle = "Private proposal",
                    state = ProposalApplicationState(
                        phase = ProposalApplicationPhase.REVIEW_READY,
                        message = "Review complete.",
                        activeProposalId = PROPOSAL_ID,
                        preview = sensitivePreview,
                    ),
                    onDismiss = {},
                    onRegenerate = {},
                    onApply = {},
                )
            }
        }

        composeRule.onAllNodesWithText("After: Concealed").assertCountEquals(2)
        composeRule.onNodeWithTag("proposal_reveal_sensitive_values").performClick()
        composeRule.onNodeWithText("After: \"Focus task\"").assertIsDisplayed()
        composeRule.onNodeWithText("After: \"Private exact note\"").assertIsDisplayed()
    }

    private fun preview() = RemoteProposalApplicationPreview(
        previewId = PREVIEW_ID,
        proposals = listOf(ProposalPreviewMember(PROPOSAL_ID, 1)),
        changeSetSchema = "dayweave.proposal-change-set/1",
        commandIds = listOf(COMMAND_ID),
        reviewHash = "sha256:${"a".repeat(64)}",
        expiresAt = "2099-09-01T10:00:00Z",
        canApply = true,
        maximumRisk = RemoteProposalRiskLevel.HIGH,
        requiresExplicitApproval = true,
        diffs = listOf(
            RemoteProposalItemDiff(
                commandId = COMMAND_ID,
                operation = RemoteProposalOperation.CREATE_ITEM,
                itemId = ITEM_ID,
                changedFields = listOf(RemoteProposalItemField.TITLE),
                before = null,
                after = item(),
            ),
        ),
        implicitDiffs = listOf(
            RemoteProposalImplicitItemDiff(
                itemId = IMPLICIT_ITEM_ID,
                reason = RemoteProposalImplicitChangeReason.HIERARCHY_REFRESH,
                changedFields = listOf(
                    RemoteProposalItemField.IS_EXECUTABLE,
                    RemoteProposalItemField.REVISION,
                ),
                before = item().copy(
                    id = IMPLICIT_ITEM_ID,
                    title = "Focus parent",
                    revision = 3,
                ),
                after = item().copy(
                    id = IMPLICIT_ITEM_ID,
                    title = "Focus parent",
                    isExecutable = false,
                    revision = 4,
                ),
            ),
        ),
        risks = listOf(
            RemoteProposalRisk(
                code = RemoteProposalRiskCode.CHANGES_DEADLINE,
                level = RemoteProposalRiskLevel.HIGH,
                commandId = COMMAND_ID,
                itemId = ITEM_ID,
                requiresExplicitApproval = true,
                summary = "This changes a deadline",
            ),
        ),
        conflicts = emptyList(),
    )

    private fun item() = RemoteProposalCanonicalItem(
        id = ITEM_ID,
        isSensitive = false,
        kind = RemoteProposalItemKind.TASK,
        status = RemoteProposalItemStatus.PLANNED,
        title = "Focus task",
        notes = null,
        timezoneName = "UTC",
        durationSeconds = 1_800,
        deadlineAt = null,
        earliestStartAt = null,
        recurrence = null,
        flexibleConstraints = buildJsonObject { },
        splitPolicy = buildJsonObject { put("type", "indivisible") },
        importance = 50,
        urgency = 50,
        parentId = null,
        siblingOrder = 0,
        isExecutable = true,
        revision = 1,
        createdAt = "2026-08-30T10:00:00Z",
        updatedAt = "2026-08-30T10:00:00Z",
        completedAt = null,
        deletedAt = null,
    )

    private companion object {
        const val PROPOSAL_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val PREVIEW_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val COMMAND_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val ITEM_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        const val IMPLICIT_ITEM_ID = "d1111111-1111-4111-8111-111111111111"
    }
}
