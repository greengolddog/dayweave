package com.greengolddog.dayweave.sync

import com.greengolddog.dayweave.network.ApiConnectionSnapshot
import com.greengolddog.dayweave.network.ApiCredentialStore
import com.greengolddog.dayweave.network.AuthenticatedApiConfiguration
import com.greengolddog.dayweave.network.GoogleAccountsApiException
import com.greengolddog.dayweave.network.GoogleAccountsTransport
import com.greengolddog.dayweave.network.GoogleService
import com.greengolddog.dayweave.network.RemoteGoogleAccount
import com.greengolddog.dayweave.network.RemoteGoogleAccounts
import com.greengolddog.dayweave.network.RemoteGoogleAuthorization
import com.greengolddog.dayweave.network.RemoteGoogleCleanupStatus
import com.greengolddog.dayweave.network.StartGoogleAuthorizationRequest
import java.io.IOException
import java.time.Instant
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GoogleAccountManagerTest {
    @Test
    fun appPrivacyLockFencesInFlightRefreshEvenWhenUiUnlocksBeforeResponse() = runBlocking {
        val presentationAllowed = AtomicBoolean(true)
        val credentials = FakeGoogleCredentials()
        val responseStarted = CompletableDeferred<Unit>()
        val releaseResponse = CompletableDeferred<Unit>()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account(label = "Private owner"))
            accountsStarted = responseStarted
            accountsGate = releaseResponse
        }
        val manager = manager(credentials, transport, presentationAllowed::get)

        val refresh = async { manager.refresh() }
        withTimeout(3_000) { responseStarted.await() }
        presentationAllowed.set(false)
        manager.quarantineBindingState()
        presentationAllowed.set(true)
        releaseResponse.complete(Unit)
        withTimeout(3_000) { refresh.await() }

        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertEquals(GoogleAccountPhase.NOT_CONFIGURED, manager.state.value.phase)

        transport.accountsStarted = null
        transport.accountsGate = null
        manager.refresh()
        assertEquals("Private owner", manager.state.value.accounts.single().label)
    }

    @Test
    fun appPrivacyLockFencesInFlightAuthorizationAcrossUnlock() = runBlocking {
        val presentationAllowed = AtomicBoolean(true)
        val authorizationStarted = CompletableDeferred<Unit>()
        val releaseAuthorization = CompletableDeferred<Unit>()
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply {
            this.authorizationStarted = authorizationStarted
            authorizationGate = releaseAuthorization
        }
        val manager = manager(
            FakeGoogleCredentials(),
            transport,
            presentationAllowed::get,
            journalStore = journals,
        )

        val authorization = async { manager.connectNew() }
        withTimeout(3_000) { authorizationStarted.await() }
        presentationAllowed.set(false)
        manager.quarantineBindingState()
        presentationAllowed.set(true)
        releaseAuthorization.complete(Unit)
        withTimeout(3_000) { authorization.await() }

        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertEquals(GoogleAccountPhase.NOT_CONFIGURED, manager.state.value.phase)

        transport.authorizationStarted = null
        transport.authorizationGate = null
        manager.restartAuthorization()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertEquals(2, transport.authorizationRequests.size)
    }

    @Test
    fun appPrivacyLockFencesInFlightAccountMutationAcrossUnlock() = runBlocking {
        val presentationAllowed = AtomicBoolean(true)
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account(label = "Private owner"))
        }
        val manager = manager(credentials, transport, presentationAllowed::get)
        manager.refresh()
        val mutationStarted = CompletableDeferred<Unit>()
        val releaseMutation = CompletableDeferred<Unit>()
        transport.pauseStarted = mutationStarted
        transport.pauseGate = releaseMutation

        val mutation = async { manager.setPaused(ACCOUNT_ID, paused = true) }
        withTimeout(3_000) { mutationStarted.await() }
        presentationAllowed.set(false)
        manager.quarantineBindingState()
        presentationAllowed.set(true)
        releaseMutation.complete(Unit)
        withTimeout(3_000) { mutation.await() }

        assertEquals(1, transport.accountsCalls)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertEquals(GoogleAccountPhase.NOT_CONFIGURED, manager.state.value.phase)
    }

    @Test
    fun lockedManagerDoesNotStartProviderOperations() = runBlocking {
        val presentationAllowed = AtomicBoolean(false)
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(FakeGoogleCredentials(), transport, presentationAllowed::get)

        manager.refresh()
        manager.connectNew()
        manager.reauthorize(ACCOUNT_ID)

        assertEquals(0, transport.accountsCalls)
        assertTrue(transport.authorizationRequests.isEmpty())
        assertEquals(GoogleAccountPhase.NOT_CONFIGURED, manager.state.value.phase)
    }

    @Test
    fun delayedOldBindingAccountsCannotReappearAfterGenerationFence() = runBlocking {
        val credentials = GenerationBoundCredentialStore()
        val responseStarted = CompletableDeferred<Unit>()
        val releaseResponse = CompletableDeferred<Unit>()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            accountsStarted = responseStarted
            accountsGate = releaseResponse
        }
        val manager = manager(credentials, transport)

        val oldRequest = async { manager.refresh() }
        withTimeout(3_000) { responseStarted.await() }
        val fence = async {
            credentials.invalidateBeforeQuarantine {
                manager.quarantineBindingState()
                true
            }
        }
        yield()
        releaseResponse.complete(Unit)

        withTimeout(3_000) { oldRequest.await() }
        assertTrue(withTimeout(3_000) { fence.await() })
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertEquals(GoogleAccountPhase.NOT_CONFIGURED, manager.state.value.phase)
        manager.refresh()
        assertTrue(manager.state.value.accounts.isEmpty())
    }

    @Test
    fun readerCreatedDuringWriterCannotSendOrRestoreOldGoogleBinding() = runBlocking {
        val credentials = GenerationBoundCredentialStore()
        val writerEntered = CompletableDeferred<Unit>()
        val releaseWriter = CompletableDeferred<Unit>()
        val configurationObserved = CompletableDeferred<Unit>()
        credentials.configurationObserved = { configurationObserved.complete(Unit) }
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)

        val fence = async {
            credentials.invalidateBeforeQuarantine {
                writerEntered.complete(Unit)
                releaseWriter.await()
                manager.quarantineBindingState()
                true
            }
        }
        withTimeout(3_000) { writerEntered.await() }
        val refresh = async { manager.refresh() }
        withTimeout(3_000) { configurationObserved.await() }

        assertTrue(credentials.enabled)
        assertEquals(0, transport.accountsCalls)
        releaseWriter.complete(Unit)

        assertTrue(withTimeout(3_000) { fence.await() })
        withTimeout(3_000) { refresh.await() }
        assertEquals(0, transport.accountsCalls)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertEquals(GoogleAccountPhase.NOT_CONFIGURED, manager.state.value.phase)
    }

    @Test
    fun refreshMapsCapabilitiesAndTruthfulRecoveryState() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)

        manager.refresh()
        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        val mapped = manager.state.value.accounts.single()
        assertTrue(mapped.hasCalendar)
        assertFalse(mapped.hasCalendarWriteScope)
        assertTrue(mapped.hasTasks)
        assertFalse(mapped.hasTasksWriteScope)
        assertTrue(mapped.isDefault)

        transport.accountsResult = accounts(
            account(calendarWrite = true, tasksWrite = true, revision = 8),
        )
        manager.refresh()
        val upgraded = manager.state.value.accounts.single()
        assertTrue(upgraded.hasCalendar)
        assertTrue(upgraded.hasCalendarWriteScope)
        assertTrue(upgraded.hasTasks)
        assertTrue(upgraded.hasTasksWriteScope)

        transport.accountsResult = accounts(account(revision = 9)).copy(
            cleanup = cleanup().copy(
                operatorRecoveryRequired = true,
                uncertainAuthorizations = 1,
            ),
        )
        manager.refresh()
        assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
        assertTrue(manager.state.value.message.contains("owner attention"))
    }

    @Test
    fun nilAccountIdentityFailsClosedBeforeAuthorizationCanStart() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val nilAccountId = "00000000-0000-0000-0000-000000000000"
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account(id = nilAccountId))
        }
        val manager = manager(
            FakeGoogleCredentials(),
            transport,
            journalStore = journals,
        )

        manager.refresh()
        manager.enableCalendarPublishing(nilAccountId)

        assertEquals(GoogleAccountPhase.ERROR, manager.state.value.phase)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertTrue(transport.authorizationRequests.isEmpty())
        assertNull(journals.journal)
    }

    @Test
    fun everyServerCleanupRecoveryFenceBlocksAuthorizationBeforeJournalOrHttp() = runBlocking {
        val recoveryStatuses = listOf(
            cleanup().copy(operatorRecoveryRequired = true),
            cleanup().copy(durabilityDegraded = true),
            cleanup().copy(revocationFenced = true),
            cleanup().copy(exhausted = 1),
            cleanup().copy(uncertainAuthorizations = 1),
            cleanup().copy(legacyRecoveryRequired = 1),
        )
        recoveryStatuses.forEach { cleanupStatus ->
            val journals = InMemoryGoogleAuthorizationJournalStore()
            val transport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account()).copy(cleanup = cleanupStatus)
            }
            val manager = manager(
                FakeGoogleCredentials(),
                transport,
                journalStore = journals,
            )
            manager.refresh()
            assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)

            manager.enableCalendarPublishing(ACCOUNT_ID)
            manager.enableTasksPublishing(ACCOUNT_ID)
            manager.reauthorize(ACCOUNT_ID)
            manager.connectNew()
            manager.restartAuthorization()

            assertEquals(0, journals.saveCalls)
            assertTrue(transport.authorizationRequests.isEmpty())
            assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
        }
    }

    @Test
    fun serverCleanupRecoveryAlsoBlocksAnAlreadySavedExactRetry() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) }
        val manager = manager(FakeGoogleCredentials(), transport, journalStore = journals)
        manager.refresh()
        manager.enableTasksPublishing(ACCOUNT_ID)
        assertEquals(1, transport.authorizationRequests.size)

        transport.accountsResult = accounts(account()).copy(
            cleanup = cleanup().copy(revocationFenced = true),
        )
        manager.refresh()
        assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
        manager.restartAuthorization()

        assertEquals(1, transport.authorizationRequests.size)
        assertNotNull(journals.journal)
        assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
    }

    @Test
    fun connectNewExposesOnlyAnExactTrustedGoogleAuthorizationUrl() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            authorizationResult = RemoteGoogleAuthorization(
                authorizationUrl =
                    "https://accounts.google.com/o/oauth2/v2/auth?state=opaque&code_challenge=proof",
                expiresAt = NOW.plusSeconds(600).toString(),
            )
        }
        val manager = manager(credentials, transport)

        manager.connectNew()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertNotNull(manager.state.value.authorization)
        assertFalse(manager.state.value.toString().contains("state=opaque"))
        val request = requireNotNull(transport.authorizationRequests.single())
        assertTrue(request.services.isEmpty())
        assertFalse(request.forceConsent)
        assertFalse(request.connectNew)
        assertTrue(request.makeDefault)

        val unsafeTransport = FakeGoogleAccountsTransport().apply {
            authorizationResult = authorizationResult.copy(
            authorizationUrl = "https://accounts.google.com.evil.example/o/oauth2/v2/auth?state=x",
            )
        }
        val unsafeManager = manager(credentials, unsafeTransport)
        unsafeManager.connectNew()
        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, unsafeManager.state.value.phase)
        assertNull(unsafeManager.state.value.authorization)
        assertNotNull(unsafeManager.state.value.authorizationRecovery)
    }

    @Test
    fun additionalAccountConnectionKeepsReadOnlySentinelAndUsesConnectNew() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()

        manager.connectNew()

        val request = transport.authorizationRequests.single()
        assertTrue(request.services.isEmpty())
        assertFalse(request.forceConsent)
        assertTrue(request.connectNew)
        assertFalse(request.makeDefault)
        assertNull(request.accountId)
    }

    @Test
    fun calendarPublishingInventoryCannotRetireTheExactBrowserAttempt() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(FakeGoogleCredentials(), transport, journalStore = journals)
        manager.refresh()

        manager.enableCalendarPublishing(ACCOUNT_ID)

        val request = transport.authorizationRequests.single()
        assertEquals(listOf(GoogleService.CALENDAR), request.services)
        assertTrue(request.forceConsent)
        assertEquals(ACCOUNT_ID, request.accountId)
        assertFalse(request.connectNew)
        assertEquals(
            GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING,
            requireNotNull(journals.journal).action,
        )

        // A projected scope without the callback's revision advance is not completion.
        transport.accountsResult = accounts(account(calendarWrite = true, revision = 7))
        manager.refresh()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertNotNull(journals.journal)

        // Nor is a newer revision that granted a different service.
        transport.accountsResult = accounts(account(tasksWrite = true, revision = 8))
        manager.refresh()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertNotNull(journals.journal)

        transport.accountsResult = accounts(account(calendarWrite = true, revision = 9))
        manager.refresh()

        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertTrue(manager.state.value.accounts.single().hasCalendarWriteScope)
        assertNotNull(manager.state.value.authorizationRecovery)
        assertNotNull(journals.journal)
    }

    @Test
    fun upgradedResponseFromReplacedCredentialCannotRetireOldAuthorizationJournal() =
        runBlocking {
            val journals = InMemoryGoogleAuthorizationJournalStore()
            val credentials = FakeGoogleCredentials()
            val transport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account())
            }
            val manager = manager(credentials, transport, journalStore = journals)
            manager.refresh()
            manager.enableCalendarPublishing(ACCOUNT_ID)

            transport.accountsResult = accounts(account(calendarWrite = true, revision = 8))
            transport.accountsHook = { credentials.configurationId = "configuration-b" }
            manager.refresh()

            assertNotNull(journals.journal)
            assertEquals("configuration-b", manager.state.value.configurationId)
            assertTrue(manager.state.value.accounts.isEmpty())
        }

    @Test
    fun tasksPublishingCanRepairReauthorizationButCalendarUpgradeCannot() = runBlocking {
        val tasksTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(
                account(status = "reauthorization_required", tasksWrite = true),
            )
        }
        val tasksManager = manager(FakeGoogleCredentials(), tasksTransport)
        tasksManager.refresh()

        tasksManager.enableTasksPublishing(ACCOUNT_ID)

        val tasksRequest = tasksTransport.authorizationRequests.single()
        assertEquals(listOf(GoogleService.TASKS), tasksRequest.services)
        assertTrue(tasksRequest.forceConsent)
        assertEquals(
            GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING,
            requireNotNull(tasksManager.state.value.authorizationRecovery).action,
        )
        tasksTransport.accountsResult = accounts(
            account(status = "active", tasksWrite = true, revision = 8),
        )
        tasksManager.refresh()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, tasksManager.state.value.phase)
        assertNotNull(tasksManager.state.value.authorizationRecovery)

        val calendarTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account(status = "reauthorization_required"))
        }
        val calendarManager = manager(FakeGoogleCredentials(), calendarTransport)
        calendarManager.refresh()
        calendarManager.enableCalendarPublishing(ACCOUNT_ID)

        assertTrue(calendarTransport.authorizationRequests.isEmpty())
        assertEquals(GoogleAccountPhase.ERROR, calendarManager.state.value.phase)
    }

    @Test
    fun lostStartResponseSurvivesProcessDeathAndRetriesExactServiceAndIdentity() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val firstTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            authorizationError = IOException("lost response")
        }
        val firstManager = manager(
            FakeGoogleCredentials(),
            firstTransport,
            journalStore = journals,
        )
        firstManager.refresh()
        firstManager.enableTasksPublishing(ACCOUNT_ID)

        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, firstManager.state.value.phase)
        val saved = requireNotNull(journals.journal)
        assertEquals(listOf(GoogleService.TASKS), saved.request.services)
        assertEquals(firstTransport.authorizationIdempotencyKeys.single(), saved.idempotencyKey)

        // A new manager models process death: no provider URL survives, but the exact request does.
        val recoveredTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val recoveredManager = manager(
            FakeGoogleCredentials(),
            recoveredTransport,
            journalStore = journals,
        )
        recoveredManager.refresh()
        assertNull(recoveredManager.state.value.authorization)
        assertEquals(
            GoogleAuthorizationAction.ENABLE_TASKS_PUBLISHING,
            requireNotNull(recoveredManager.state.value.authorizationRecovery).action,
        )

        recoveredManager.restartAuthorization()

        assertEquals(listOf(GoogleService.TASKS), recoveredTransport.authorizationRequests.single().services)
        assertEquals(saved.idempotencyKey, recoveredTransport.authorizationIdempotencyKeys.single())
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, recoveredManager.state.value.phase)

        recoveredTransport.accountsResult = accounts(account(tasksWrite = true, revision = 8))
        recoveredManager.refresh()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, recoveredManager.state.value.phase)
        assertNotNull(journals.journal)
    }

    @Test
    fun persistBeforeSendAndActionAwareAdmissionFailClosed() = runBlocking {
        val deniedActions = mutableListOf<Pair<GoogleAuthorizationAction, String?>>()
        val deniedTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val deniedManager = manager(
            FakeGoogleCredentials(),
            deniedTransport,
            authorizationMutationAllowed = { action, targetAccountId ->
                deniedActions += action to targetAccountId
                false
            },
        )
        deniedManager.refresh()
        deniedManager.enableCalendarPublishing(ACCOUNT_ID)
        assertEquals(
            listOf(GoogleAuthorizationAction.ENABLE_CALENDAR_PUBLISHING to ACCOUNT_ID),
            deniedActions,
        )
        assertTrue(deniedTransport.authorizationRequests.isEmpty())

        val unsaved = InMemoryGoogleAuthorizationJournalStore().apply { failSave = true }
        val unsavedTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val unsavedManager = manager(
            FakeGoogleCredentials(),
            unsavedTransport,
            journalStore = unsaved,
        )
        unsavedManager.refresh()
        unsavedManager.enableTasksPublishing(ACCOUNT_ID)
        assertTrue(unsavedTransport.authorizationRequests.isEmpty())
        assertNull(unsaved.journal)

        val allowed = AtomicBoolean(true)
        val fenced = InMemoryGoogleAuthorizationJournalStore().apply {
            saveHook = { allowed.set(false) }
        }
        val fencedTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val fencedManager = manager(
            FakeGoogleCredentials(),
            fencedTransport,
            authorizationMutationAllowed = { _, _ -> allowed.get() },
            journalStore = fenced,
        )
        fencedManager.refresh()
        fencedManager.enableCalendarPublishing(ACCOUNT_ID)
        assertNotNull(fenced.journal)
        assertTrue(fencedTransport.authorizationRequests.isEmpty())
        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, fencedManager.state.value.phase)
    }

    @Test
    fun reauthorizationAdmissionCarriesExactTargetForFreshRetryAndBrowserHandoff() =
        runBlocking {
            val observed = mutableListOf<Pair<GoogleAuthorizationAction, String?>>()
            val journals = InMemoryGoogleAuthorizationJournalStore()
            val transport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(
                    account(status = "reauthorization_required"),
                    account(
                        id = SECOND_ACCOUNT_ID,
                        label = "Other",
                        status = "reauthorization_required",
                        isDefault = false,
                    ),
                )
            }
            val manager = manager(
                FakeGoogleCredentials(),
                transport,
                authorizationMutationAllowed = { action, targetAccountId ->
                    observed += action to targetAccountId
                    action == GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY &&
                        targetAccountId == ACCOUNT_ID
                },
                journalStore = journals,
            )
            manager.refresh()

            manager.reauthorize(SECOND_ACCOUNT_ID)

            assertEquals(0, journals.saveCalls)
            assertTrue(transport.authorizationRequests.isEmpty())
            assertEquals(
                GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY to SECOND_ACCOUNT_ID,
                observed.single(),
            )

            manager.reauthorize(ACCOUNT_ID)
            assertEquals(1, journals.saveCalls)
            assertEquals(ACCOUNT_ID, transport.authorizationRequests.single().accountId)
            manager.restartAuthorization()
            assertEquals(2, transport.authorizationRequests.size)
            assertTrue(transport.authorizationRequests.all { it.accountId == ACCOUNT_ID })

            val url = requireNotNull(manager.state.value.authorization).url
            var opened = false
            assertTrue(manager.useAuthorizationUrlIfCurrent(url) { opened = true })
            assertTrue(opened)
            assertTrue(
                observed.drop(1).all { (action, targetAccountId) ->
                    action == GoogleAuthorizationAction.REAUTHORIZE_READ_ONLY &&
                        targetAccountId == ACCOUNT_ID
                },
            )
        }

    @Test
    fun browserHandoffPersistsOpenedBeforeFinalFenceAndNeverInvokesBlockedConsumer() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        var browserPhase = false
        var browserAdmissionChecks = 0
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(
            FakeGoogleCredentials(),
            transport,
            authorizationMutationAllowed = { _, _ ->
                if (!browserPhase) {
                    true
                } else {
                    browserAdmissionChecks += 1
                    browserAdmissionChecks == 1
                }
            },
            journalStore = journals,
        )
        manager.refresh()
        manager.enableCalendarPublishing(ACCOUNT_ID)
        val url = requireNotNull(manager.state.value.authorization).url
        browserPhase = true

        // The first browser check passes. The callback closes before the post-CAS final check.
        val opened = manager.useAuthorizationUrlIfCurrent(url) {
            error("blocked consumer must not run")
        }

        assertFalse(opened)
        assertEquals(2, browserAdmissionChecks)
        assertTrue(requireNotNull(journals.journal).browserOpened)
        assertNull(manager.state.value.authorization)
    }

    @Test
    fun browserHandoffToleratesPermittedBackwardClockAdjustment() = runBlocking {
        var observedNow = NOW
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(
            FakeGoogleCredentials(),
            transport,
            journalStore = journals,
            nowProvider = { observedNow },
        )
        manager.refresh()
        manager.enableCalendarPublishing(ACCOUNT_ID)
        val url = requireNotNull(manager.state.value.authorization).url
        observedNow = NOW.minusSeconds(120)
        var openedUrl: String? = null

        assertTrue(manager.useAuthorizationUrlIfCurrent(url) { openedUrl = it })

        assertEquals(url, openedUrl)
        assertEquals(NOW.toEpochMilli(), requireNotNull(journals.journal).browserOpenedAtEpochMillis)
    }

    @Test
    fun authorizationRecoveryBlockerRetiresOnlySafelyExpiredRecords() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) }
        val manager = manager(FakeGoogleCredentials(), transport, journalStore = journals)
        manager.refresh()
        manager.enableTasksPublishing(ACCOUNT_ID)
        assertTrue(manager.hasAuthorizationRecoveryBlocker())

        val active = requireNotNull(journals.journal)
        journals.journal = active.copy(
            createdAtEpochMillis = NOW.minusSeconds(1_800).toEpochMilli(),
            expiresAtEpochMillis = NOW.minusSeconds(1).toEpochMilli(),
        )
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
        assertNotNull(journals.journal)
        journals.journal = requireNotNull(journals.journal).copy(
            createdAtEpochMillis = NOW.minusSeconds(1_800).toEpochMilli(),
            expiresAtEpochMillis = NOW.minusMillis(
                GoogleAuthorizationJournal.SAFE_RETIREMENT_DELAY_MILLIS + 1,
            ).toEpochMilli(),
        )
        assertFalse(manager.hasAuthorizationRecoveryBlocker())
        assertNull(journals.journal)

        journals.corrupt = true
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
    }

    @Test
    fun rawServerExpiryCannotReleaseBindingDuringClockSkewOrCallbackSettlement() = runBlocking {
        var observedNow = NOW
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) }
        val manager = manager(
            FakeGoogleCredentials(),
            transport,
            journalStore = journals,
            nowProvider = { observedNow },
        )
        manager.refresh()
        manager.enableCalendarPublishing(ACCOUNT_ID)
        val serverExpiry = Instant.parse(transport.authorizationResult.expiresAt)

        observedNow = serverExpiry
        manager.refresh()

        assertTrue(manager.hasAuthorizationRecoveryBlocker())
        assertNotNull(journals.journal)
        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, manager.state.value.phase)
        assertTrue(requireNotNull(manager.state.value.authorizationRecovery).browserWindowExpired)

        observedNow = serverExpiry.plusMillis(
            GoogleAuthorizationJournal.SAFE_RETIREMENT_DELAY_MILLIS,
        )
        manager.refresh()

        assertFalse(manager.hasAuthorizationRecoveryBlocker())
        assertNull(journals.journal)
        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
    }

    @Test
    fun directPauseAndDisconnectFailClosedWhileAuthorizationJournalExists() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) }
        val manager = manager(FakeGoogleCredentials(), transport, journalStore = journals)
        manager.refresh()
        manager.enableCalendarPublishing(ACCOUNT_ID)

        manager.setPaused(ACCOUNT_ID, paused = true)
        manager.disconnect(ACCOUNT_ID)

        assertEquals(0, transport.pauseCalls)
        assertEquals(0, transport.disconnectCalls)
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, manager.state.value.phase)
    }

    @Test
    fun unreadableRecoveryRequiresManagerIssuedExplicitResetConfirmation() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore().apply { corrupt = true }
        val transport = FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) }
        val manager = manager(FakeGoogleCredentials(), transport, journalStore = journals)
        assertNull(manager.unreadableAuthorizationRecoveryResetConfirmation())

        manager.refresh()

        assertEquals(GoogleAccountPhase.ERROR, manager.state.value.phase)
        assertTrue(manager.state.value.authorizationRecoveryResetRequired)
        assertEquals(1, transport.accountsCalls)
        val confirmation = requireNotNull(
            manager.unreadableAuthorizationRecoveryResetConfirmation(),
        )
        manager.resetUnreadableAuthorizationRecovery(confirmation)
        assertFalse(manager.hasAuthorizationRecoveryBlocker())
        assertFalse(manager.state.value.authorizationRecoveryResetRequired)
    }

    @Test
    fun confirmedLocalDestructionClearsExactlyOrFailsClosed() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) }
        val manager = manager(FakeGoogleCredentials(), transport, journalStore = journals)
        manager.refresh()
        manager.enableTasksPublishing(ACCOUNT_ID)
        assertTrue(manager.hasAuthorizationRecoveryBlocker())

        journals.failClear = true
        assertFalse(manager.abandonAuthorizationForConfirmedLocalDestruction())
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
        assertNotNull(journals.journal)

        journals.failClear = false
        assertTrue(manager.abandonAuthorizationForConfirmedLocalDestruction())
        assertFalse(manager.hasAuthorizationRecoveryBlocker())
        assertNull(journals.journal)
        assertTrue(manager.state.value.accounts.isEmpty())
    }

    @Test
    fun orphanedAuthorizationSurfacesWithoutCredentialsAndDiscardsOnlyAfterConfirmation() =
        runBlocking {
            val journals = InMemoryGoogleAuthorizationJournalStore()
            val credentials = FakeGoogleCredentials()
            val initialTransport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account())
            }
            val initialManager = manager(
                credentials,
                initialTransport,
                journalStore = journals,
            )
            initialManager.refresh()
            initialManager.enableTasksPublishing(ACCOUNT_ID)
            assertNotNull(journals.journal)

            // Simulate process death followed by loss of the encrypted credential binding.
            credentials.hasBearerToken = false
            val recoveredTransport = FakeGoogleAccountsTransport()
            val recoveredManager = manager(
                credentials,
                recoveredTransport,
                journalStore = journals,
            )
            recoveredManager.refresh()

            val state = recoveredManager.state.value
            assertEquals(0, recoveredTransport.accountsCalls)
            assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, state.phase)
            assertTrue(state.requiresPlannerApiConfiguration)
            assertTrue(state.authorizationRecoveryDiscardRequired)
            assertFalse(state.authorizationRecoveryResetRequired)
            assertNull(state.authorization)
            assertNull(state.authorizationRecovery)
            assertFalse(state.toString().contains(ACCOUNT_ID))
            assertFalse(state.toString().contains("ENABLE_TASKS_PUBLISHING"))

            val confirmation = requireNotNull(
                recoveredManager.authorizationRecoveryDiscardConfirmation(),
            )
            assertEquals(
                "GoogleAuthorizationRecoveryDiscardConfirmation(<redacted>)",
                confirmation.toString(),
            )
            assertTrue(recoveredManager.discardAuthorizationRecovery(confirmation))
            assertNull(journals.journal)
            assertFalse(recoveredManager.hasAuthorizationRecoveryBlocker())
            assertFalse(recoveredManager.state.value.authorizationRecoveryDiscardRequired)
        }

    @Test
    fun sameBaseUrlDoesNotProveForeignJournalIdentityAndStaleExactDiscardFailsClosed() =
        runBlocking {
            val journals = InMemoryGoogleAuthorizationJournalStore()
            val credentials = FakeGoogleCredentials()
            val initialTransport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account())
            }
            val initialManager = manager(
                credentials,
                initialTransport,
                journalStore = journals,
            )
            initialManager.refresh()
            initialManager.enableCalendarPublishing(ACCOUNT_ID)
            val saved = requireNotNull(journals.journal)

            // URL is unchanged, but this is a different credential generation.
            credentials.configurationId = "configuration-b"
            val recoveredTransport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account(label = "Current binding"))
            }
            val recoveredManager = manager(
                credentials,
                recoveredTransport,
                journalStore = journals,
            )
            recoveredManager.refresh()

            assertEquals(1, recoveredTransport.accountsCalls)
            assertTrue(recoveredManager.state.value.authorizationRecoveryDiscardRequired)
            assertNull(recoveredManager.state.value.authorizationRecovery)
            assertTrue(credentials.hasBearerToken)
            val confirmation = requireNotNull(
                recoveredManager.authorizationRecoveryDiscardConfirmation(),
            )

            // A different exact record cannot be removed by the old capability.
            journals.journal = saved.copy(
                browserOpenedAtEpochMillis = NOW.plusSeconds(1).toEpochMilli(),
            )
            assertFalse(recoveredManager.discardAuthorizationRecovery(confirmation))
            assertNotNull(journals.journal)
            assertTrue(credentials.hasBearerToken)
        }

    @Test
    fun discardConfirmationIsBoundToExactCredentialSnapshot() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val credentials = FakeGoogleCredentials()
        val initialTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val initialManager = manager(credentials, initialTransport, journalStore = journals)
        initialManager.refresh()
        initialManager.enableCalendarPublishing(ACCOUNT_ID)

        credentials.configurationId = "configuration-b"
        val recoveredManager = manager(
            credentials,
            FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) },
            journalStore = journals,
        )
        recoveredManager.refresh()
        val confirmation = requireNotNull(
            recoveredManager.authorizationRecoveryDiscardConfirmation(),
        )

        credentials.configurationId = "configuration-c"
        assertFalse(recoveredManager.discardAuthorizationRecovery(confirmation))
        assertNotNull(journals.journal)
        assertTrue(recoveredManager.hasAuthorizationRecoveryBlocker())
    }

    @Test
    fun corruptOrphanUsesDistinctVerifiedClearAndRetainsCredentialsOnFailure() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore().apply {
            corrupt = true
            failClear = true
        }
        val credentials = FakeGoogleCredentials().apply { hasBearerToken = false }
        val transport = FakeGoogleAccountsTransport()
        val manager = manager(credentials, transport, journalStore = journals)

        manager.refresh()

        assertEquals(0, transport.accountsCalls)
        assertTrue(manager.state.value.authorizationRecoveryDiscardRequired)
        assertTrue(manager.state.value.authorizationRecoveryResetRequired)
        assertNull(manager.state.value.authorizationRecovery)
        val confirmation = requireNotNull(manager.authorizationRecoveryDiscardConfirmation())
        assertFalse(manager.discardAuthorizationRecovery(confirmation))
        assertTrue(journals.corrupt)
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
        assertFalse(credentials.hasBearerToken)

        journals.failClear = false
        assertTrue(manager.discardAuthorizationRecovery(confirmation))
        assertFalse(journals.corrupt)
        assertFalse(manager.hasAuthorizationRecoveryBlocker())
    }

    @Test
    fun corruptOrphanDiscardCannotClearAReplacementArtifact() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore().apply { corrupt = true }
        val credentials = FakeGoogleCredentials().apply { hasBearerToken = false }
        val manager = manager(
            credentials,
            FakeGoogleAccountsTransport(),
            journalStore = journals,
        )
        manager.refresh()
        val stale = requireNotNull(manager.authorizationRecoveryDiscardConfirmation())
        journals.corruptArtifactVersion += 1

        assertFalse(manager.discardAuthorizationRecovery(stale))
        assertTrue(journals.corrupt)
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
    }

    @Test
    fun expiredOrphanThatCannotAutoClearRequiresExactDiscardNotCorruptReset() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val credentials = FakeGoogleCredentials()
        val initialTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val initialManager = manager(credentials, initialTransport, journalStore = journals)
        initialManager.refresh()
        initialManager.enableTasksPublishing(ACCOUNT_ID)
        journals.journal = requireNotNull(journals.journal).copy(
            createdAtEpochMillis = NOW.minusSeconds(1_800).toEpochMilli(),
            expiresAtEpochMillis = NOW.minusSeconds(1).toEpochMilli(),
            browserOpenedAtEpochMillis = null,
        )
        journals.failRemove = true
        credentials.hasBearerToken = false
        val recoveredManager = manager(
            credentials,
            FakeGoogleAccountsTransport(),
            journalStore = journals,
        )

        recoveredManager.refresh()

        assertTrue(recoveredManager.state.value.authorizationRecoveryDiscardRequired)
        assertFalse(recoveredManager.state.value.authorizationRecoveryResetRequired)
        assertNull(recoveredManager.unreadableAuthorizationRecoveryResetConfirmation())
        val confirmation = requireNotNull(
            recoveredManager.authorizationRecoveryDiscardConfirmation(),
        )
        assertFalse(recoveredManager.discardAuthorizationRecovery(confirmation))
        assertNotNull(journals.journal)

        journals.failRemove = false
        assertTrue(recoveredManager.discardAuthorizationRecovery(confirmation))
        assertNull(journals.journal)
    }

    @Test
    fun exactDiscardRechecksPrivacyAfterJournalLoadBeforeRemoval() = runBlocking {
        val allowed = AtomicBoolean(true)
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val credentials = FakeGoogleCredentials()
        val initialTransport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val initialManager = manager(credentials, initialTransport, journalStore = journals)
        initialManager.refresh()
        initialManager.enableTasksPublishing(ACCOUNT_ID)

        credentials.hasBearerToken = false
        val recoveredManager = manager(
            credentials,
            FakeGoogleAccountsTransport(),
            operationAllowed = allowed::get,
            journalStore = journals,
        )
        recoveredManager.refresh()
        val confirmation = requireNotNull(
            recoveredManager.authorizationRecoveryDiscardConfirmation(),
        )
        journals.loadHook = { allowed.set(false) }

        assertFalse(recoveredManager.discardAuthorizationRecovery(confirmation))
        assertNotNull(journals.journal)
        assertTrue(recoveredManager.hasAuthorizationRecoveryBlocker())
    }

    @Test
    fun corruptResetRechecksCredentialSnapshotAfterJournalLoadBeforeClear() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore().apply { corrupt = true }
        val credentials = FakeGoogleCredentials()
        val manager = manager(
            credentials,
            FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) },
            journalStore = journals,
        )
        manager.refresh()
        val confirmation = requireNotNull(
            manager.unreadableAuthorizationRecoveryResetConfirmation(),
        )
        journals.loadHook = { credentials.configurationId = "configuration-b" }

        manager.resetUnreadableAuthorizationRecovery(confirmation)

        assertTrue(journals.corrupt)
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
    }

    @Test
    fun corruptResetConfirmationCannotClearAReplacementArtifact() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore().apply { corrupt = true }
        val manager = manager(
            FakeGoogleCredentials(),
            FakeGoogleAccountsTransport().apply { accountsResult = accounts(account()) },
            journalStore = journals,
        )
        manager.refresh()
        val stale = requireNotNull(manager.unreadableAuthorizationRecoveryResetConfirmation())
        journals.corruptArtifactVersion += 1

        manager.resetUnreadableAuthorizationRecovery(stale)

        assertTrue(journals.corrupt)
        assertTrue(manager.hasAuthorizationRecoveryBlocker())
    }

    @Test
    fun duplicateScopeProjectionFailsClosedWithoutPublishingCapabilities() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val duplicate = "https://www.googleapis.com/auth/calendar.readonly"
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(
                account(grantedScopes = listOf("openid", "email", duplicate, duplicate)),
            )
        }
        val manager = manager(credentials, transport)

        manager.refresh()

        assertEquals(GoogleAccountPhase.ERROR, manager.state.value.phase)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
    }

    @Test
    fun responseFromReplacedApiCredentialsCannotRebindGoogleState() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            accountsHook = { credentials.configurationId = "configuration-b" }
        }
        val manager = manager(credentials, transport)

        manager.refresh()

        assertTrue(manager.state.value.accounts.isEmpty())
        assertEquals(GoogleAccountPhase.DISCONNECTED, manager.state.value.phase)
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun optimisticConflictRefreshesAuthoritativeAccountInsteadOfGuessing() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            pauseError = GoogleAccountsApiException.Conflict()
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.accountsResult = accounts(account().copy(status = "paused", syncEnabled = false, revision = 8))

        manager.setPaused(ACCOUNT_ID, paused = true)

        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        assertEquals("paused", manager.state.value.accounts.single().status)
        assertFalse(manager.state.value.accounts.single().syncEnabled)
        assertEquals(2, transport.accountsCalls)
    }

    @Test
    fun rejectedPlannerCredentialOffersTheConfigurationFlow() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsError = GoogleAccountsApiException.Authentication()
        }
        val manager = manager(credentials, transport)

        manager.refresh()

        assertEquals(GoogleAccountPhase.AUTH_REQUIRED, manager.state.value.phase)
        assertTrue(manager.state.value.requiresPlannerApiConfiguration)
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun activeAccountRemainsUsableWhenAnotherAccountNeedsReauthorization() = runBlocking {
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(
                account(id = ACCOUNT_ID, isDefault = true),
                account(
                    id = SECOND_ACCOUNT_ID,
                    label = "Needs repair",
                    status = "reauthorization_required",
                    isDefault = false,
                ),
            )
        }
        val manager = manager(FakeGoogleCredentials(), transport)

        manager.refresh()

        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        assertEquals(2, manager.state.value.accounts.size)
        assertTrue(manager.state.value.message.contains("need authorization"))
    }

    @Test
    fun activeAccountRemainsUsableWhenAnotherDisconnectNeedsRetry() = runBlocking {
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(
                account(id = ACCOUNT_ID, isDefault = true),
                account(
                    id = SECOND_ACCOUNT_ID,
                    label = "Disconnect pending",
                    status = "revocation_failed",
                    isDefault = false,
                ),
            )
        }
        val manager = manager(FakeGoogleCredentials(), transport)

        manager.refresh()

        assertEquals(GoogleAccountPhase.CONNECTED, manager.state.value.phase)
        assertEquals(2, manager.state.value.accounts.size)
        assertTrue(manager.state.value.message.contains("Disconnect"))
    }

    @Test
    fun credentialReplacementDropsCachedAccountsAndAuthorizationBeforeRefresh() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            authorizationResult = RemoteGoogleAuthorization(
                "https://accounts.google.com/o/oauth2/v2/auth?state=credential-a",
                NOW.plusSeconds(600).toString(),
            )
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()
        val oldUrl = requireNotNull(manager.state.value.authorization).url
        var openedUrl: String? = null
        assertTrue(manager.useAuthorizationUrlIfCurrent(oldUrl) { openedUrl = it })
        assertEquals(oldUrl, openedUrl)

        credentials.configurationId = "configuration-b"
        transport.accountsResult = accounts()
        manager.refresh()

        assertEquals("configuration-b", manager.state.value.configurationId)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertFalse(manager.useAuthorizationUrlIfCurrent(oldUrl) { error("must not open") })
    }

    @Test
    fun serializedCredentialReplacementClearsCachedIdentityBeforeUnlocking() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()

        manager.withConfigurationChangeLock {
            credentials.configurationId = "configuration-b"
        }

        assertEquals("configuration-b", manager.state.value.configurationId)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        val requestsBeforeRestart = transport.authorizationRequests.size
        manager.restartAuthorization()
        assertEquals(requestsBeforeRestart, transport.authorizationRequests.size)
    }

    @Test
    fun partialCredentialClearWithSameGenerationRejectsCachedAuthorizationUrl() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()
        val oldUrl = requireNotNull(manager.state.value.authorization).url

        manager.withConfigurationChangeLock {
            // Models Keystore deletion followed by a failed preference clear.
            credentials.hasBearerToken = false
        }

        assertEquals("configuration-a", manager.state.value.configurationId)
        assertEquals(GoogleAccountPhase.NOT_CONFIGURED, manager.state.value.phase)
        assertNull(manager.state.value.authorization)
        assertFalse(manager.useAuthorizationUrlIfCurrent(oldUrl) { error("must not open") })
    }

    @Test
    fun browserHandoffWaitsForCredentialReplacementAndThenRejectsOldUrl() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()
        val oldUrl = requireNotNull(manager.state.value.authorization).url
        val replacementEntered = CountDownLatch(1)
        val allowReplacement = CountDownLatch(1)
        val replacement = async(Dispatchers.Default) {
            manager.withConfigurationChangeLock {
                replacementEntered.countDown()
                check(allowReplacement.await(2, TimeUnit.SECONDS))
                credentials.configurationId = "configuration-b"
            }
        }
        assertTrue(replacementEntered.await(2, TimeUnit.SECONDS))
        // UNDISPATCHED reaches the already-held mutex before returning, proving this handoff is
        // actually queued behind the credential replacement rather than merely scheduled later.
        val browserUse = async(start = CoroutineStart.UNDISPATCHED) {
            manager.useAuthorizationUrlIfCurrent(oldUrl) { error("must not open") }
        }

        allowReplacement.countDown()
        replacement.await()

        assertFalse(browserUse.await())
        assertEquals("configuration-b", manager.state.value.configurationId)
    }

    @Test
    fun staleAccountActionNeverLeavesUnderReplacementCredential() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        credentials.configurationId = "configuration-b"

        manager.reauthorize(ACCOUNT_ID)

        assertTrue(transport.authorizationRequests.isEmpty())
        assertTrue(manager.state.value.accounts.isEmpty())
        assertEquals("configuration-b", manager.state.value.configurationId)
    }

    @Test
    fun conflictRefreshCannotApplyResponseAfterCredentialReplacement() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
            pauseError = GoogleAccountsApiException.Conflict()
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.accountsHook = { credentials.configurationId = "configuration-b" }

        manager.setPaused(ACCOUNT_ID, paused = true)

        assertTrue(manager.state.value.accounts.isEmpty())
        assertEquals("configuration-b", manager.state.value.configurationId)
        assertEquals(GoogleAccountPhase.DISCONNECTED, manager.state.value.phase)
    }

    @Test
    fun configurationReplacingBetweenSnapshotAndDecryptionCannotSendARequest() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        credentials.configurationHook = { credentials.configurationId = "configuration-b" }

        manager.refresh()

        assertEquals(0, transport.accountsCalls)
        assertEquals("configuration-b", manager.state.value.configurationId)
        assertTrue(manager.state.value.accounts.isEmpty())
    }

    @Test
    fun accountInventoryCannotCorrelateAndClearAConnectAttempt() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)

        transport.accountsResult = accounts(
            account(),
            account(id = SECOND_ACCOUNT_ID, label = "Second", isDefault = false),
        )
        manager.refresh()

        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertNotNull(manager.state.value.authorization)
        assertNotNull(manager.state.value.authorizationRecovery)
        assertEquals(2, manager.state.value.accounts.size)
    }

    @Test
    fun readOnlyConnectRequiresOneUnambiguousNewAccountWithBothServiceCapabilities() =
        runBlocking {
            val journals = InMemoryGoogleAuthorizationJournalStore()
            val transport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts()
            }
            val manager = manager(
                FakeGoogleCredentials(),
                transport,
                journalStore = journals,
            )
            manager.refresh()
            manager.connectNew()

            transport.accountsResult = accounts(
                account(),
                account(
                    id = SECOND_ACCOUNT_ID,
                    label = "Concurrent account",
                    isDefault = false,
                ),
            )
            manager.refresh()

            assertNotNull(journals.journal)
            assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)

            transport.accountsResult = accounts(account())
            manager.refresh()

            assertNotNull(journals.journal)
            assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        }

    @Test
    fun reauthorizationInventoryChangeRetainsExactRecoveryAndCanRetry() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account(status = "reauthorization_required"))
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.reauthorize(ACCOUNT_ID)
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        val firstRequest = transport.authorizationRequests.single()
        assertTrue(firstRequest.services.isEmpty())
        assertTrue(firstRequest.forceConsent)
        assertEquals(ACCOUNT_ID, firstRequest.accountId)
        assertFalse(firstRequest.connectNew)
        assertTrue(firstRequest.makeDefault)

        // An unchanged authoritative account means denial/failure is not guessed as success.
        manager.refresh()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        manager.restartAuthorization()
        assertEquals(2, transport.authorizationRequests.size)
        assertEquals(firstRequest, transport.authorizationRequests.last())

        transport.accountsResult = accounts(account(status = "active", revision = 8))
        manager.refresh()
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
        assertNotNull(manager.state.value.authorization)
        assertNotNull(manager.state.value.authorizationRecovery)
    }

    @Test
    fun readOnlyReauthorizationInventoryNeverCorrelatesTheBrowserAttempt() = runBlocking {
        val journals = InMemoryGoogleAuthorizationJournalStore()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account(status = "reauthorization_required"))
        }
        val manager = manager(
            FakeGoogleCredentials(),
            transport,
            journalStore = journals,
        )
        manager.refresh()
        manager.reauthorize(ACCOUNT_ID)

        transport.accountsResult = accounts(
            account(
                status = "active",
                revision = 8,
                grantedScopes = listOf("openid", "email"),
            ),
        )
        manager.refresh()
        assertNotNull(journals.journal)

        transport.accountsResult = accounts(
            account(
                status = "active",
                revision = 9,
                grantedScopes = listOf(
                    "openid",
                    "email",
                    "https://www.googleapis.com/auth/calendar.readonly",
                ),
            ),
        )
        manager.refresh()
        assertNotNull(journals.journal)

        transport.accountsResult = accounts(account(status = "active", revision = 10))
        manager.refresh()
        assertNotNull(journals.journal)
        assertEquals(GoogleAccountPhase.AWAITING_BROWSER, manager.state.value.phase)
    }

    @Test
    fun browserOpenFailureAfterDurableHandoffCanOnlyCheckStatus() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        manager.connectNew()

        val url = requireNotNull(manager.state.value.authorization).url
        val openError = runCatching {
            manager.useAuthorizationUrlIfCurrent(url) { throw IllegalStateException("no browser") }
        }
        assertTrue(openError.exceptionOrNull() is IllegalStateException)
        manager.browserOpenFailed()

        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, manager.state.value.phase)
        assertNull(manager.state.value.authorization)
        assertTrue(requireNotNull(manager.state.value.authorizationRecovery).browserOpened)
        manager.restartAuthorization()
        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, manager.state.value.phase)
        assertEquals(1, transport.authorizationRequests.size)
    }

    @Test
    fun expiredAuthorizationResponseRetainsExactRecoveryWithoutOpenableUrl() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            authorizationResult = RemoteGoogleAuthorization(
                "https://accounts.google.com/o/oauth2/v2/auth?state=expired",
                NOW.minusSeconds(1).toString(),
            )
        }
        val manager = manager(credentials, transport)

        manager.connectNew()

        assertEquals(GoogleAccountPhase.AUTHORIZATION_RECOVERY, manager.state.value.phase)
        assertNull(manager.state.value.authorization)
        assertNotNull(manager.state.value.authorizationRecovery)
    }

    @Test
    fun failedDisconnectReconcilesAndNeverClaimsAccessWasRevoked() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.disconnectError = GoogleAccountsApiException.Unavailable()
        transport.accountsResult = accounts(
            account(status = "revocation_failed", revision = 8),
        )

        manager.disconnect(ACCOUNT_ID)

        assertEquals(GoogleAccountPhase.ERROR, manager.state.value.phase)
        assertTrue(manager.state.value.message.contains("not confirmed revoked"))
        assertEquals("revocation_failed", manager.state.value.accounts.single().status)
        assertEquals(2, transport.accountsCalls)
    }

    @Test
    fun cancelledAmbiguousDisconnectAlwaysLeavesRefreshableNonBusyState() = runBlocking {
        listOf<Exception>(
            GoogleAccountsApiException.Unavailable(),
            GoogleAccountsApiException.Http(404),
        ).forEach { disconnectFailure ->
            val credentials = FakeGoogleCredentials()
            val transport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account())
            }
            val manager = manager(credentials, transport)
            manager.refresh()
            transport.disconnectError = disconnectFailure
            transport.accountsError = CancellationException("cancel reconciliation")

            try {
                manager.disconnect(ACCOUNT_ID)
                error("cancellation must propagate")
            } catch (_: CancellationException) {
                // Expected: cancellation stays structured, while durable UI state is repaired.
            }

            assertEquals(GoogleAccountPhase.ERROR, manager.state.value.phase)
            assertFalse(manager.state.value.isBusy)
            assertTrue(manager.state.value.message.contains("outcome is unknown"))
        }
    }

    @Test
    fun ambiguousDisconnectPreservesMandatoryOperatorRecovery() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.disconnectError = GoogleAccountsApiException.Unavailable()
        transport.accountsResult = accounts(account()).copy(
            cleanup = cleanup().copy(operatorRecoveryRequired = true),
        )

        manager.disconnect(ACCOUNT_ID)

        assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
        assertTrue(manager.state.value.message.contains("owner attention"))
        assertFalse(manager.state.value.isBusy)
    }

    @Test
    fun failedOrCancelledDisconnectReconciliationCannotHideExistingRecoveryFence() = runBlocking {
        listOf(
            CancellationException("synthetic cancellation") to true,
            IOException("synthetic reconciliation failure") to false,
        ).forEach { (reconciliationFailure, shouldCancel) ->
            val credentials = FakeGoogleCredentials()
            val transport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account()).copy(
                    cleanup = cleanup().copy(operatorRecoveryRequired = true),
                )
            }
            val manager = manager(credentials, transport)
            manager.refresh()
            assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
            transport.disconnectError = GoogleAccountsApiException.Unavailable()
            transport.accountsError = reconciliationFailure

            val result = runCatching { manager.disconnect(ACCOUNT_ID) }

            assertEquals(shouldCancel, result.exceptionOrNull() is CancellationException)
            assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
            assertTrue(manager.state.value.message.contains("owner attention"))
            assertFalse(manager.state.value.isBusy)
        }
    }

    @Test
    fun directOrConflictDisconnectFailureCannotHideExistingRecoveryFence() = runBlocking {
        listOf(
            Triple(CancellationException("synthetic direct cancellation"), null, true),
            Triple(IOException("synthetic direct failure"), null, false),
            Triple(
                GoogleAccountsApiException.Conflict(),
                CancellationException("synthetic conflict refresh cancellation"),
                true,
            ),
            Triple(
                GoogleAccountsApiException.Conflict(),
                IOException("synthetic conflict refresh failure"),
                false,
            ),
        ).forEach { (disconnectFailure, refreshFailure, shouldCancel) ->
            val credentials = FakeGoogleCredentials()
            val transport = FakeGoogleAccountsTransport().apply {
                accountsResult = accounts(account()).copy(
                    cleanup = cleanup().copy(operatorRecoveryRequired = true),
                )
            }
            val manager = manager(credentials, transport)
            manager.refresh()
            assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
            transport.disconnectError = disconnectFailure
            transport.accountsError = refreshFailure

            val result = runCatching { manager.disconnect(ACCOUNT_ID) }

            assertEquals(shouldCancel, result.exceptionOrNull() is CancellationException)
            assertEquals(GoogleAccountPhase.RECOVERY_REQUIRED, manager.state.value.phase)
            assertTrue(manager.state.value.message.contains("owner attention"))
            assertFalse(manager.state.value.isBusy)
        }
    }

    @Test
    fun failedDisconnectReconciliationNeverRestoresStateFromReplacedCredentials() = runBlocking {
        val credentials = FakeGoogleCredentials()
        val transport = FakeGoogleAccountsTransport().apply {
            accountsResult = accounts(account())
        }
        val manager = manager(credentials, transport)
        manager.refresh()
        transport.disconnectError = GoogleAccountsApiException.Unavailable()
        transport.accountsError = IOException("synthetic reconciliation failure")
        transport.accountsHook = { credentials.configurationId = "configuration-b" }

        manager.disconnect(ACCOUNT_ID)

        assertEquals("configuration-b", manager.state.value.configurationId)
        assertEquals(GoogleAccountPhase.DISCONNECTED, manager.state.value.phase)
        assertTrue(manager.state.value.accounts.isEmpty())
        assertNull(manager.state.value.authorization)
        assertFalse(manager.state.value.isBusy)
    }

    private fun manager(
        credentials: ApiCredentialStore,
        transport: FakeGoogleAccountsTransport,
        operationAllowed: () -> Boolean = { true },
        authorizationMutationAllowed: (GoogleAuthorizationAction, String?) -> Boolean =
            { _, _ -> true },
        journalStore: InMemoryGoogleAuthorizationJournalStore =
            InMemoryGoogleAuthorizationJournalStore(),
        nowProvider: () -> Instant = { NOW },
    ) = GoogleAccountManager(
        credentialStore = credentials,
        transport = transport,
        now = nowProvider,
        newUuid = { UUID.fromString(IDEMPOTENCY_KEY) },
        operationAllowed = operationAllowed,
        authorizationMutationAllowed = authorizationMutationAllowed,
        authorizationJournalStore = journalStore,
    )

    private fun accounts(vararg accounts: RemoteGoogleAccount) = RemoteGoogleAccounts(
        accounts = accounts.toList(),
        cleanup = cleanup(),
    )

    private fun cleanup() = RemoteGoogleCleanupStatus(
        held = 0,
        pending = 0,
        retrying = 0,
        exhausted = 0,
        volatileGuardians = 0,
        durabilityDegraded = false,
        revocationFenced = false,
        operatorRecoveryRequired = false,
        uncertainAuthorizations = 0,
        legacyRecoveryRequired = 0,
        nextAttemptAt = null,
        lastFailureAt = null,
    )

    private fun account(
        id: String = ACCOUNT_ID,
        label: String = "Owner",
        status: String = "active",
        isDefault: Boolean = true,
        revision: Long = 7,
        calendarWrite: Boolean = false,
        tasksWrite: Boolean = false,
        grantedScopes: List<String>? = null,
    ) = RemoteGoogleAccount(
        id = id,
        externalAccountId = "google-$id",
        displayLabel = label,
        status = status,
        syncEnabled = status == "active",
        isDefault = isDefault,
        grantedScopes = grantedScopes ?: listOf(
            "openid",
            "email",
            if (calendarWrite) {
                "https://www.googleapis.com/auth/calendar"
            } else {
                "https://www.googleapis.com/auth/calendar.readonly"
            },
            if (tasksWrite) {
                "https://www.googleapis.com/auth/tasks"
            } else {
                "https://www.googleapis.com/auth/tasks.readonly"
            },
        ),
        tokenExpiresAt = NOW.plusSeconds(3_600).toString(),
        revision = revision,
        createdAt = NOW.minusSeconds(3_600).toString(),
        updatedAt = NOW.toString(),
    )

    private companion object {
        val NOW: Instant = Instant.parse("2026-09-01T07:00:00Z")
        const val ACCOUNT_ID = "11111111-1111-4111-8111-111111111111"
        const val SECOND_ACCOUNT_ID = "33333333-3333-4333-8333-333333333333"
        const val IDEMPOTENCY_KEY = "22222222-2222-4222-8222-222222222222"
    }
}

private class FakeGoogleCredentials : ApiCredentialStore {
    var configurationId = "configuration-a"
    var hasBearerToken = true
    var configurationHook: (() -> Unit)? = null

    override fun snapshot() = ApiConnectionSnapshot(
        baseUrl = "https://api.example.test/",
        hasBearerToken = hasBearerToken,
        lastSuccessfulSyncEpochMillis = null,
        configurationId = configurationId,
    )

    override fun authenticatedConfiguration(): AuthenticatedApiConfiguration? {
        configurationHook?.also { configurationHook = null }?.invoke()
        if (!hasBearerToken) return null
        return AuthenticatedApiConfiguration.createBound(
            "https://api.example.test/",
            "test-secret",
            configurationId,
        )
    }

    override fun update(baseUrl: String, bearerToken: String?) = Unit
    override fun clear() = Unit
    override fun recordSuccessfulSync(epochMillis: Long) = Unit
}

private class FakeGoogleAccountsTransport : GoogleAccountsTransport {
    var accountsResult = RemoteGoogleAccounts(emptyList(), emptyCleanup())
    var authorizationResult = RemoteGoogleAuthorization(
        "https://accounts.google.com/o/oauth2/v2/auth?state=opaque",
        "2026-09-01T07:10:00Z",
    )
    var pauseError: Exception? = null
    var disconnectError: Exception? = null
    var accountsError: Exception? = null
    var authorizationError: Exception? = null
    var accountsHook: (() -> Unit)? = null
    var accountsStarted: CompletableDeferred<Unit>? = null
    var accountsGate: CompletableDeferred<Unit>? = null
    var authorizationStarted: CompletableDeferred<Unit>? = null
    var authorizationGate: CompletableDeferred<Unit>? = null
    var pauseStarted: CompletableDeferred<Unit>? = null
    var pauseGate: CompletableDeferred<Unit>? = null
    var accountsCalls = 0
    var pauseCalls = 0
    var disconnectCalls = 0
    val authorizationRequests = mutableListOf<StartGoogleAuthorizationRequest>()
    val authorizationIdempotencyKeys = mutableListOf<String>()

    override suspend fun accounts(
        configuration: AuthenticatedApiConfiguration,
    ): RemoteGoogleAccounts {
        accountsCalls += 1
        accountsStarted?.complete(Unit)
        accountsGate?.await()
        accountsHook?.invoke()
        accountsError?.let { throw it }
        return accountsResult
    }

    override suspend fun startAuthorization(
        configuration: AuthenticatedApiConfiguration,
        idempotencyKey: String,
        request: StartGoogleAuthorizationRequest,
    ): RemoteGoogleAuthorization {
        authorizationRequests += request
        authorizationIdempotencyKeys += idempotencyKey
        authorizationStarted?.complete(Unit)
        authorizationGate?.await()
        authorizationError?.let { throw it }
        return authorizationResult
    }

    override suspend fun setPaused(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        paused: Boolean,
        idempotencyKey: String,
    ): RemoteGoogleAccount {
        pauseCalls += 1
        pauseStarted?.complete(Unit)
        pauseGate?.await()
        pauseError?.let { throw it }
        return accountsResult.accounts.single()
    }

    override suspend fun disconnect(
        configuration: AuthenticatedApiConfiguration,
        accountId: String,
        expectedRevision: Long,
        idempotencyKey: String,
    ): RemoteGoogleAccount {
        disconnectCalls += 1
        disconnectError?.let { throw it }
        return accountsResult.accounts.single()
    }

    companion object {
        private fun emptyCleanup() = RemoteGoogleCleanupStatus(
            held = 0,
            pending = 0,
            retrying = 0,
            exhausted = 0,
            volatileGuardians = 0,
            durabilityDegraded = false,
            revocationFenced = false,
            operatorRecoveryRequired = false,
            uncertainAuthorizations = 0,
            legacyRecoveryRequired = 0,
            nextAttemptAt = null,
            lastFailureAt = null,
        )
    }
}

private class InMemoryGoogleAuthorizationJournalStore : GoogleAuthorizationJournalStore {
    var journal: GoogleAuthorizationJournal? = null
    var corrupt = false
    var corruptArtifactVersion = 1
    var failSave = false
    var failUpdate = false
    var failRemove = false
    var failClear = false
    var saveHook: (() -> Unit)? = null
    var loadHook: (() -> Unit)? = null
    var saveCalls = 0

    override fun load(nowEpochMillis: Long): GoogleAuthorizationJournalLoadResult {
        loadHook?.also { loadHook = null }?.invoke()
        if (corrupt) {
            return GoogleAuthorizationJournalLoadResult.Corrupt(
                GoogleAuthorizationCorruptArtifactIdentity("test-corrupt-$corruptArtifactVersion"),
            )
        }
        val current = journal ?: return GoogleAuthorizationJournalLoadResult.Empty
        return if (current.isValidAt(nowEpochMillis)) {
            GoogleAuthorizationJournalLoadResult.Loaded(current)
        } else if (current.isSafeToRetireAt(nowEpochMillis)) {
            GoogleAuthorizationJournalLoadResult.Retirable(current)
        } else {
            GoogleAuthorizationJournalLoadResult.Expired(current)
        }
    }

    override fun saveIfAbsent(
        journal: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean {
        saveCalls += 1
        if (failSave || this.journal != null || !journal.isValidAt(nowEpochMillis)) return false
        this.journal = journal
        saveHook?.invoke()
        return true
    }

    override fun updateExact(
        expected: GoogleAuthorizationJournal,
        replacement: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean {
        if (failUpdate || journal != expected || !replacement.isValidAt(nowEpochMillis)) return false
        journal = replacement
        return true
    }

    override fun removeExact(
        expected: GoogleAuthorizationJournal,
        nowEpochMillis: Long,
    ): Boolean {
        if (failRemove || journal != expected) return false
        journal = null
        return true
    }

    override fun clearForConfirmedReset(nowEpochMillis: Long): Boolean {
        if (failClear) return false
        corrupt = false
        journal = null
        return true
    }

    override fun clearCorruptExact(
        expected: GoogleAuthorizationCorruptArtifactIdentity,
        nowEpochMillis: Long,
    ): Boolean {
        if (
            failClear || !corrupt ||
            expected != GoogleAuthorizationCorruptArtifactIdentity(
                "test-corrupt-$corruptArtifactVersion",
            )
        ) {
            return false
        }
        corrupt = false
        journal = null
        return true
    }
}
