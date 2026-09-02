package com.greengolddog.dayweave.ui.authoring

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.greengolddog.dayweave.model.GoogleCalendarOutboundPreviewSnapshot
import com.greengolddog.dayweave.model.GoogleCalendarOutboundTarget
import com.greengolddog.dayweave.network.GoogleCalendarOutboundEntityKind
import com.greengolddog.dayweave.network.GoogleCalendarOutboundOperation
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundApprovalConfirmation
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundPhase
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundState
import com.greengolddog.dayweave.sync.GoogleCalendarOutboundTargetOption
import com.greengolddog.dayweave.ui.theme.DayWeaveTheme
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class GoogleCalendarOutboundReviewSheetTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun multipleDestinationsRequireAnExplicitChoiceBeforePreview() {
        val personal = targetOption(
            accountId = ACCOUNT_ID,
            collectionId = COLLECTION_ID,
            displayName = "Personal · Focus",
        )
        val work = targetOption(
            accountId = OTHER_ACCOUNT_ID,
            collectionId = OTHER_COLLECTION_ID,
            displayName = "Work · Delivery",
        )
        val requested = AtomicReference<GoogleCalendarOutboundTarget?>()

        composeRule.setContent {
            var selected by remember {
                mutableStateOf<GoogleCalendarOutboundTargetOption?>(null)
            }
            DayWeaveTheme(useDynamicColor = false) {
                GoogleCalendarOutboundReviewSheet(
                    state = readyState(),
                    targets = listOf(personal, work),
                    selectedTarget = selected,
                    approvalConfirmation = null,
                    canRecover = false,
                    canDiscardExpiredRecovery = false,
                    onTargetSelected = { selected = it },
                    onRequestPreview = requested::set,
                    onApproveAndQueue = {},
                    onRecover = {},
                    onDiscardExpiredRecovery = {},
                    onDismissRequest = {},
                )
            }
        }

        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_PREVIEW_BUTTON_TAG).assertIsNotEnabled()
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_DESTINATION_PICKER_TAG).performClick()
        composeRule.onNodeWithTag("google_outbound_destination_1").performClick()
        composeRule.onNodeWithText("Work · Delivery").assertExists()
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_PREVIEW_BUTTON_TAG)
            .assertIsEnabled()
            .performClick()

        composeRule.runOnIdle { assertEquals(work.target, requested.get()) }
    }

    @Test
    fun reviewShowsOnlyAllowlistedPreviewFieldsAndRoutesOpaqueApproval() {
        val preview = preview()
        val confirmation = GoogleCalendarOutboundApprovalConfirmation(
            recoveryId = RECOVERY_ID,
            operationGeneration = 8,
            configurationId = "config-1",
            previewId = PREVIEW_ID,
            previewHash = PREVIEW_HASH,
        )
        val approved = AtomicReference<GoogleCalendarOutboundApprovalConfirmation?>()

        composeRule.setContent {
            DayWeaveTheme(useDynamicColor = false) {
                GoogleCalendarOutboundReviewSheet(
                    state = GoogleCalendarOutboundState(
                        phase = GoogleCalendarOutboundPhase.AWAITING_APPROVAL,
                        message = "Review the exact private Calendar change.",
                        preview = preview,
                        hasPendingRecovery = true,
                        configurationId = "config-1",
                    ),
                    targets = emptyList(),
                    selectedTarget = null,
                    reviewDestinationDisplayName = "Private Gmail · Private calendar",
                    approvalConfirmation = confirmation,
                    canRecover = false,
                    canDiscardExpiredRecovery = false,
                    onTargetSelected = {},
                    onRequestPreview = {},
                    onApproveAndQueue = approved::set,
                    onRecover = {},
                    onDiscardExpiredRecovery = {},
                    onDismissRequest = {},
                )
            }
        }

        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_DESTINATION_TAG)
            .assertTextContains("Private Gmail · Private calendar", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_CHANGE_TAG)
            .assertTextContains("Create new event", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_TITLE_TAG)
            .assertTextContains("Architecture focus", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_DESCRIPTION_TAG)
            .assertTextContains("Prepare the launch plan", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_START_TAG)
            .assertTextContains("10:00", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_END_TAG)
            .assertTextContains("11:30", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_STATUS_TAG)
            .assertTextContains("Confirmed", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_TRANSPARENCY_TAG)
            .assertTextContains("Busy", substring = true)
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_EXPIRY_TAG)
            .assertTextContains("2030", substring = true)

        listOf(
            "extendedProperties",
            OWNERSHIP_PROOF_CANARY,
            PREVIEW_HASH,
        ).forEach { forbidden ->
            composeRule.onAllNodesWithText(
                forbidden,
                substring = true,
                useUnmergedTree = true,
            ).assertCountEquals(0)
            composeRule.onAllNodesWithContentDescription(
                forbidden,
                substring = true,
                useUnmergedTree = true,
            ).assertCountEquals(0)
        }

        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_APPROVE_BUTTON_TAG).performClick()
        composeRule.runOnIdle { assertSame(confirmation, approved.get()) }

        assertEquals(
            "Update existing event",
            preview.copy(providerResourceId = "provider-id", providerEtag = "provider-etag")
                .toSanitizedOutboundPresentation()
                ?.change,
        )
    }

    @Test
    fun recoveryAndDiscardActionsExistOnlyWhenHostAllowsThem() {
        val recoverAllowed = mutableStateOf(false)
        val discardAllowed = mutableStateOf(false)
        val recovered = AtomicBoolean(false)
        val discarded = AtomicBoolean(false)

        composeRule.setContent {
            DayWeaveTheme(useDynamicColor = false) {
                GoogleCalendarOutboundReviewSheet(
                    state = GoogleCalendarOutboundState(
                        phase = GoogleCalendarOutboundPhase.RESPONSE_UNKNOWN,
                        message = "The one-time approval response is unknown.",
                        hasPendingRecovery = true,
                        configurationId = "config-1",
                    ),
                    targets = emptyList(),
                    selectedTarget = null,
                    approvalConfirmation = null,
                    canRecover = recoverAllowed.value,
                    canDiscardExpiredRecovery = discardAllowed.value,
                    onTargetSelected = {},
                    onRequestPreview = {},
                    onApproveAndQueue = {},
                    onRecover = { recovered.set(true) },
                    onDiscardExpiredRecovery = { discarded.set(true) },
                    onDismissRequest = {},
                )
            }
        }

        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_RECOVERY_CARD_TAG).assertExists()
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_RECOVER_BUTTON_TAG).assertDoesNotExist()
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_DISCARD_BUTTON_TAG).assertDoesNotExist()

        composeRule.runOnIdle { recoverAllowed.value = true }
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_RECOVER_BUTTON_TAG).performClick()
        composeRule.runOnIdle { assertTrue(recovered.get()) }
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_DISCARD_BUTTON_TAG).assertDoesNotExist()

        composeRule.runOnIdle { discardAllowed.value = true }
        composeRule.onNodeWithTag(GOOGLE_OUTBOUND_DISCARD_BUTTON_TAG).performClick()
        composeRule.runOnIdle { assertTrue(discarded.get()) }
    }

    @Test
    fun dialogPolicyAlwaysForcesSecureAndBusyPreventsDismissal() {
        val ordinary = googleCalendarOutboundDialogProperties(isBusy = false)
        val busy = googleCalendarOutboundDialogProperties(isBusy = true)

        assertEquals(SecureFlagPolicy.SecureOn, ordinary.securePolicy)
        assertEquals(SecureFlagPolicy.SecureOn, busy.securePolicy)
        assertTrue(ordinary.dismissOnBackPress)
        assertTrue(ordinary.dismissOnClickOutside)
        assertEquals(false, busy.dismissOnBackPress)
        assertEquals(false, busy.dismissOnClickOutside)
    }

    private fun readyState() = GoogleCalendarOutboundState(
        phase = GoogleCalendarOutboundPhase.READY,
        message = "Choose a destination and review its private Calendar change.",
        configurationId = "config-1",
    )

    private fun targetOption(
        accountId: String,
        collectionId: String,
        displayName: String,
    ) = GoogleCalendarOutboundTargetOption(
        target = GoogleCalendarOutboundTarget(
            accountId = accountId,
            collectionId = collectionId,
            collectionRevision = 4,
        ),
        displayName = displayName,
    )

    private fun preview() = GoogleCalendarOutboundPreviewSnapshot(
        id = PREVIEW_ID,
        accountId = ACCOUNT_ID,
        collectionId = COLLECTION_ID,
        collectionRevision = 4,
        collectionDisplayName = "Private calendar",
        itemId = ITEM_ID,
        itemRevision = 7,
        entityKind = GoogleCalendarOutboundEntityKind.CALENDAR_EVENT,
        operation = GoogleCalendarOutboundOperation.UPSERT,
        previewHash = PREVIEW_HASH,
        providerPayload = Json.parseToJsonElement(
            """
            {
              "id":"$PROVIDER_EVENT_ID",
              "etag":null,
              "summary":"Architecture focus",
              "description":"Prepare the launch plan",
              "location":null,
              "status":"confirmed",
              "transparency":"opaque",
              "visibility":"private",
              "eventType":"default",
              "start":{"date":null,"dateTime":"2030-09-02T10:00:00+02:00","timeZone":"Europe/Paris"},
              "end":{"date":null,"dateTime":"2030-09-02T11:30:00+02:00","timeZone":"Europe/Paris"},
              "attendees":[],
              "attachments":[],
              "recurrence":[],
              "conferenceData":null,
              "recurringEventId":null,
              "originalStartTime":null,
              "updated":null,
              "sequence":null,
              "extendedProperties":{
                "private":{"dayweaveOwnershipProof":"$OWNERSHIP_PROOF_CANARY"},
                "shared":{}
              }
            }
            """.trimIndent(),
        ) as JsonObject,
        expiresAt = "2030-09-02T12:00:00Z",
    )

    private companion object {
        const val ACCOUNT_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        const val OTHER_ACCOUNT_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        const val COLLECTION_ID = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        const val OTHER_COLLECTION_ID = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
        const val ITEM_ID = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        const val PREVIEW_ID = "ffffffff-ffff-4fff-8fff-ffffffffffff"
        const val RECOVERY_ID = "12121212-1212-4212-8212-121212121212"
        val PREVIEW_HASH = "a".repeat(64)
        val PROVIDER_EVENT_ID = "d1" + "a".repeat(64)
        const val OWNERSHIP_PROOF_CANARY = "[server-managed]"
    }
}
