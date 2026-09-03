package com.greengolddog.dayweave.model

import java.io.File
import kotlinx.serialization.json.long
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import java.util.UUID
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class CanonicalAuthoringModelsTest {
    private val fixtureJson = Json { ignoreUnknownKeys = false }

    @Test
    fun recoverableCanonicalTrashUsesTheAcceptedThirtyDayWindow() {
        assertEquals(30L * 24L * 60L * 60L, CanonicalTrashRetentionPolicy.RETENTION_SECONDS)
    }

    @Test
    fun allSixKindsHaveAValidTypedInboxOrPlannedDraft() {
        val drafts = listOf(
            taskDraft(),
            taskDraft().copy(
                kind = ItemKind.HABIT,
                recurrence = CanonicalRecurrenceDraft(
                    kind = CanonicalRecurrenceKind.DAILY,
                    occurrencesPerPeriod = 2,
                ),
            ),
            taskDraft().copy(
                kind = ItemKind.ROUTINE,
                recurrence = CanonicalRecurrenceDraft(
                    kind = CanonicalRecurrenceKind.WEEKLY,
                    occurrencesPerPeriod = 3,
                    weekdays = listOf(
                        CanonicalWeekday.MONDAY,
                        CanonicalWeekday.WEDNESDAY,
                        CanonicalWeekday.FRIDAY,
                    ),
                ),
            ),
            taskDraft().copy(
                kind = ItemKind.GOAL,
                durationSeconds = null,
                split = CanonicalSplitDraft(),
            ),
            eventDraft(),
            taskDraft().copy(kind = ItemKind.BREAK, split = CanonicalSplitDraft()),
        )

        drafts.forEach { it.requireValid(ITEM_ID) }
        assertEquals(
            setOf(ItemKind.TASK, ItemKind.HABIT, ItemKind.ROUTINE, ItemKind.GOAL,
                ItemKind.EVENT, ItemKind.BREAK),
            drafts.map { it.kind }.toSet(),
        )
        assertEquals(
            setOf(CanonicalDraftPlacement.INBOX, CanonicalDraftPlacement.PLANNED),
            drafts.map { it.placement }.toSet(),
        )
    }

    @Test
    fun recurrenceSplitHierarchyAndEventBoundsFailClosed() {
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(
                placement = CanonicalDraftPlacement.PLANNED,
                kind = ItemKind.HABIT,
                recurrence = null,
            ).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(
                durationSeconds = 1_800,
                split = CanonicalSplitDraft(
                    kind = CanonicalSplitKind.SPLITTABLE,
                    minimumChunkSeconds = 900,
                    maximumChunkSeconds = 3_600,
                ),
            ).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(parentId = ITEM_ID).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            eventDraft().copy(deadlineAt = "2026-08-30T11:30:00Z").requireValid(ITEM_ID)
        }
    }

    @Test
    fun intervalRecurrenceUsesWholeMinuteWireUnits() {
        val recurrence = CanonicalRecurrenceDraft(
            kind = CanonicalRecurrenceKind.EVERY_INTERVAL,
            intervalSeconds = 2 * 60 * 60,
        )
        assertEquals(120L, recurrence.toCanonicalJson().getValue("interval").jsonPrimitive.long)
        assertThrows(IllegalArgumentException::class.java) {
            recurrence.copy(intervalSeconds = 90).requireValid()
        }
        val maximum = recurrence.copy(
            intervalSeconds = MAX_SCHEDULING_OFFSET_MINUTES * 60L,
        )
        maximum.requireValid()
        assertThrows(IllegalArgumentException::class.java) {
            maximum.copy(
                intervalSeconds = (MAX_SCHEDULING_OFFSET_MINUTES + 1) * 60L,
            ).requireValid()
        }

        val draft = taskDraft().copy(recurrence = recurrence)
        assertEquals(recurrence, canonicalItem(draft).toCanonicalDraft().recurrence)
    }

    @Test
    fun frequencyCustomAndRichSchedulerMetadataUseComposerWireShapes() {
        val hard = CanonicalConstraintStrengthDraft.hard()
        val soft = CanonicalConstraintStrengthDraft.soft(250)
        val draft = taskDraft().copy(
            placement = CanonicalDraftPlacement.PLANNED,
            durationSeconds = 7_200,
            earliestStartAt = null,
            deadlineAt = null,
            recurrence = CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.FREQUENCY,
                occurrencesPerPeriod = 3,
                weekdays = listOf(
                    CanonicalWeekday.MONDAY,
                    CanonicalWeekday.WEDNESDAY,
                    CanonicalWeekday.FRIDAY,
                ),
                period = CanonicalRecurrencePeriod.WEEK,
                semantics = CanonicalRecurrenceSemantics.CALENDAR,
                minimumSpacingMinutes = 1_440,
            ),
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.DEEP,
                energyStrength = hard,
                tags = listOf("focus", "writing"),
                scheduling = CanonicalSchedulingConstraintsDraft(
                    earliestStart = CanonicalQualifiedInstantDraft(
                        "2026-09-03T08:00:00+02:00",
                        hard,
                    ),
                    latestFinish = CanonicalQualifiedInstantDraft(
                        "2026-09-30T18:00:00+02:00",
                        soft,
                    ),
                    minimumNotice = CanonicalQualifiedMinutesDraft(30, hard),
                    allowedWeekdays = CanonicalQualifiedWeekdaysDraft(
                        CanonicalWeekday.entries.take(5),
                        hard,
                    ),
                    preferredDailyWindows = listOf(
                        CanonicalDailyWindowDraft(
                            weekdays = listOf(CanonicalWeekday.MONDAY),
                            startMinute = 540,
                            endMinute = 720,
                            strength = CanonicalConstraintStrengthDraft.soft(125),
                        ),
                    ),
                    preferredAbsoluteWindows = listOf(
                        CanonicalAbsoluteWindowDraft(
                            "2026-09-07T09:00:00+02:00",
                            "2026-09-07T12:00:00+02:00",
                            CanonicalConstraintStrengthDraft.soft(75),
                        ),
                    ),
                    forbiddenWindows = listOf(
                        CanonicalAbsoluteWindowDraft(
                            "2026-09-09T12:00:00+02:00",
                            "2026-09-09T13:00:00+02:00",
                            hard,
                        ),
                    ),
                    requiredContexts = listOf(
                        CanonicalQualifiedStringDraft("computer", hard),
                    ),
                    requiredLocation = CanonicalQualifiedStringDraft(
                        "home",
                        CanonicalConstraintStrengthDraft.soft(40),
                    ),
                    maximumDailyWork = CanonicalQualifiedMinutesDraft(180, hard),
                    maximumWeeklyWork = CanonicalQualifiedMinutesDraft(
                        480,
                        CanonicalConstraintStrengthDraft.soft(60),
                    ),
                    buffers = CanonicalBufferPolicyDraft(
                        10,
                        15,
                        CanonicalConstraintStrengthDraft.soft(90),
                    ),
                    includesNullOccurrenceWindow = true,
                ),
                maximumSessions = 3,
                minimumGapMinutes = 30,
                maximumSplitDays = 2,
            ).normalized(),
            split = CanonicalSplitDraft(
                CanonicalSplitKind.SPLITTABLE,
                minimumChunkSeconds = 1_800,
                maximumChunkSeconds = 3_600,
            ),
        )

        draft.requireValid(ITEM_ID)
        val recurrence = draft.recurrence!!.toCanonicalJson()
        val metadata = draft.constraints.toCanonicalJson(null, draft.durationSeconds, "Europe/Madrid")
        assertEquals("frequency", recurrence.getValue("type").jsonPrimitive.content)
        assertEquals("calendar", recurrence.getValue("semantics").jsonPrimitive.content)
        assertTrue(metadata.getValue("constraints").jsonObject.containsKey("buffers"))
        assertEquals(JsonNull, metadata.getValue("constraints").jsonObject["occurrence_window"])
        assertEquals(draft, canonicalItem(draft).toCanonicalDraft())

        val custom = draft.copy(
            recurrence = CanonicalRecurrenceDraft(
                kind = CanonicalRecurrenceKind.CUSTOM,
                rrule = "FREQ=MONTHLY;BYDAY=1MO,-1FR",
            ),
        )
        custom.requireValid(ITEM_ID)
        assertEquals(custom, canonicalItem(custom).toCanonicalDraft())
    }

    @Test
    fun kindSpecificMetadataRoundTripsAndContradictionsFailClosed() {
        val habit = taskDraft().copy(
            kind = ItemKind.HABIT,
            recurrence = CanonicalRecurrenceDraft(CanonicalRecurrenceKind.DAILY, 2),
            constraints = CanonicalFlexibleConstraintsDraft(
                habitTarget = CanonicalHabitTargetDraft(8, "glasses"),
                preservesStreakWhenPaused = false,
            ),
        )
        val routine = taskDraft().copy(
            kind = ItemKind.ROUTINE,
            constraints = CanonicalFlexibleConstraintsDraft(
                routineOrdered = true,
                hasOwnEffort = true,
            ),
        )
        val goal = taskDraft().copy(
            kind = ItemKind.GOAL,
            recurrence = null,
            constraints = CanonicalFlexibleConstraintsDraft(
                hasOwnEffort = true,
                goalMeasures = listOf(CanonicalGoalMeasureDraft("chapters", 12, 3, "chapters")),
                goalWeeklyAllocation = CanonicalWeeklyAllocationDraft(120, 300),
            ),
        )
        val breakDraft = taskDraft().copy(
            kind = ItemKind.BREAK,
            recurrence = null,
            constraints = CanonicalFlexibleConstraintsDraft(
                breakCategory = CanonicalBreakCategory.MOVEMENT,
                breakMandatory = true,
                breakPromptToResume = true,
            ),
            split = CanonicalSplitDraft(),
        )

        listOf(habit, routine, goal, breakDraft).forEach { draft ->
            draft.requireValid(ITEM_ID)
            assertEquals(draft.normalized(), canonicalItem(draft).toCanonicalDraft())
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(
                constraints = CanonicalFlexibleConstraintsDraft(
                    habitTarget = CanonicalHabitTargetDraft(1, "page"),
                ),
            ).requireValid(ITEM_ID)
        }
    }

    @Test
    fun incompleteInboxAndUnknownDurationPlannedWorkRemainValidStoredStates() {
        taskDraft().copy(durationSeconds = null, split = CanonicalSplitDraft())
            .requireValid(ITEM_ID)
        taskDraft().copy(
            kind = ItemKind.HABIT,
            durationSeconds = null,
            recurrence = null,
            split = CanonicalSplitDraft(),
        ).requireValid(ITEM_ID)
        CanonicalItemDraft(
            kind = ItemKind.EVENT,
            title = "Unresolved event",
            timezoneName = "UTC",
        ).requireValid(ITEM_ID)

        val unknownDurationPlanned = taskDraft().copy(
            placement = CanonicalDraftPlacement.PLANNED,
            durationSeconds = null,
            split = CanonicalSplitDraft(),
        )
        unknownDurationPlanned.requireValid(ITEM_ID)
        assertFalse(unknownDurationPlanned.createsPlanningDemand(ITEM_ID))
        assertThrows(IllegalArgumentException::class.java) {
            CanonicalItemDraft(
                placement = CanonicalDraftPlacement.PLANNED,
                kind = ItemKind.EVENT,
                title = "Unresolved event",
                timezoneName = "UTC",
            ).requireValid(ITEM_ID)
        }
    }

    @Test
    fun everySharedValidFixtureHasAnExplicitAndroidAuthoringClassification() {
        val editable = setOf(
            "twice_daily_habit",
            "completion_relative_ordered_routine",
            "measured_goal_with_weekly_allocation",
            "prompted_movement_break",
            "legacy_daily_default_count",
            "legacy_weekly_default_count",
            "legacy_monthly_default_count",
            "inbox_habit_may_be_incomplete",
            "inbox_event_may_lack_timing",
            "indivisible_explicit_default_split_extensions",
        )
        val intentionallyReadOnly = setOf(
            // Android deliberately defers graph-link authority.
            "frequency_task_with_rich_constraints_and_split_policy",
            // These statuses are server/execution owned rather than directly authored.
            "open_ended_active_break",
            "blocking_imported_event_with_matching_canonical_bounds",
            "dst_fall_back_all_day_calendar_context",
            "owned_legacy_firm_block",
        )
        assertEquals(editable + intentionallyReadOnly, fixtureCaseNames(valid = true))

        editable.forEach { name ->
            val draft = fixtureItem(name).toCanonicalDraft()
            assertTrue("$name should be directly editable", draft.matches(fixtureItem(name)))
        }
        intentionallyReadOnly.forEach { name ->
            assertThrows("$name should remain read-only", IllegalArgumentException::class.java) {
                fixtureItem(name).toCanonicalDraft()
            }
        }

        assertEquals(
            1,
            fixtureItem("legacy_daily_default_count").toCanonicalDraft()
                .recurrence?.occurrencesPerPeriod,
        )
        assertEquals(
            2,
            fixtureItem("legacy_weekly_default_count").toCanonicalDraft()
                .recurrence?.occurrencesPerPeriod,
        )
        assertNull(fixtureItem("inbox_habit_may_be_incomplete").toCanonicalDraft().recurrence)
        val explicitSplitDefaults = fixtureItem(
            "indivisible_explicit_default_split_extensions",
        )
        PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = explicitSplitDefaults.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = explicitSplitDefaults.toCanonicalDraft(),
            expectedRevision = explicitSplitDefaults.revision,
            baseItem = explicitSplitDefaults,
            createdAt = "2026-09-03T07:00:00Z",
        ).requireValid()
    }

    @Test
    fun schemaVersionOneDraftsWithoutNewDefaultedFieldsStillDecode() {
        val json = Json { encodeDefaults = true }
        val original = CanonicalItemDraft(
            title = "Legacy capture",
            timezoneName = "UTC",
        )
        val encoded = json.encodeToJsonElement(original).jsonObject
        val legacyConstraintKeys = setOf(
            "energy",
            "tags",
            "preferredStartMinute",
            "minimumGapMinutes",
            "maximumSessions",
        )
        val legacy = JsonObject(
            encoded + ("constraints" to JsonObject(
                encoded.getValue("constraints").jsonObject.filterKeys(legacyConstraintKeys::contains),
            )),
        )

        assertEquals(original, json.decodeFromJsonElement<CanonicalItemDraft>(legacy))
    }

    @Test
    fun everySharedInvalidFixtureIsRejectedExceptTheIntentionalReadOnlyRrule() {
        val retainedReadOnly = "custom_rrule_is_retained_but_not_authorable"
        val invalid = fixtureCaseNames(valid = false)
        assertTrue(retainedReadOnly in invalid)
        val eventTimingInvariant = "planned_event_without_timing"
        (invalid - retainedReadOnly - eventTimingInvariant).forEach { name ->
            assertThrows("Expected shared fixture $name to fail", IllegalArgumentException::class.java) {
                fixtureItem(name, valid = false).toCanonicalDraft()
            }
        }

        val eventFailure = assertThrows(IllegalArgumentException::class.java) {
            // Force the directly authorable non-Inbox placement so this regression proves the
            // missing-timing invariant, rather than the server-owned-status presentation fence.
            fixtureItem(eventTimingInvariant, valid = false).copy(status = "planned")
                .toCanonicalDraft()
        }
        assertTrue(eventFailure.message.orEmpty().contains("requires timing metadata"))

        val retainedItem = fixtureItem(retainedReadOnly, valid = false)
        val retained = retainedItem.toCanonicalDraft()
        assertEquals(CanonicalRecurrenceKind.CUSTOM, retained.recurrence?.kind)
        assertEquals("FREQ=MONTHLY;BYDAY=1MO,-1FR", retained.recurrence?.rrule)
        assertThrows(IllegalArgumentException::class.java) {
            PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = retainedItem.id,
                operation = CanonicalAuthoringOperation.REPLACE,
                draft = retained,
                expectedRevision = 1,
                baseItem = retainedItem,
                createdAt = "2026-09-03T07:00:00Z",
            ).requireValid()
        }
    }

    @Test
    fun rustValidDefaultNullEmptyAndUnsortedSpellingsDecodeWithoutDataLoss() {
        val item = canonicalItem(taskDraft()).copy(
            recurrenceJson = """
                {
                  "type":"weekly",
                  "times_per_week":2,
                  "weekdays":["friday","monday"]
                }
            """.trimIndent(),
            flexibleConstraintsJson = """
                {
                  "energy":null,
                  "tags":["z,comma","a"],
                  "preferred_start_minute":null,
                  "maximum_sessions":null,
                  "maximum_split_days":null,
                  "has_own_effort":false,
                  "goal_ids":[],
                  "constraints":{
                    "earliest_start":null,
                    "latest_finish":null,
                    "minimum_notice":null,
                    "allowed_weekdays":{
                      "value":["friday","monday"],
                      "strength":{"level":"hard"}
                    },
                    "preferred_daily_windows":[{
                      "value":{
                        "weekdays":["friday","monday"],
                        "start_minute":540,
                        "end_minute":720
                      },
                      "strength":{"level":"soft","weight":0}
                    }],
                    "preferred_absolute_windows":[],
                    "forbidden_windows":[],
                    "required_contexts":[],
                    "required_location":null,
                    "dependencies":[],
                    "maximum_daily_work":null,
                    "maximum_weekly_work":null,
                    "buffers":{"before":0,"after":0,"strength":null},
                    "occurrence_window":null
                  }
                }
            """.trimIndent(),
        )

        val decoded = item.toCanonicalDraft()

        assertEquals(listOf("a", "z,comma"), decoded.constraints.tags)
        assertEquals(
            listOf(CanonicalWeekday.MONDAY, CanonicalWeekday.FRIDAY),
            decoded.constraints.scheduling?.allowedWeekdays?.value,
        )
        assertNull(decoded.constraints.scheduling?.buffers?.strength)
        assertEquals(0L, decoded.constraints.scheduling?.buffers?.beforeMinutes)
        assertTrue(decoded.constraints.scheduling?.includesNullOccurrenceWindow == true)
        assertTrue(decoded.matches(item))

        val allocation = canonicalItem(
            taskDraft().copy(
                kind = ItemKind.GOAL,
                recurrence = null,
                split = CanonicalSplitDraft(),
            ),
        ).copy(
            flexibleConstraintsJson =
                """{"goal_weekly_allocation":{"minimum":120}}""",
        ).toCanonicalDraft().constraints.goalWeeklyAllocation
        assertEquals(120L, allocation?.minimumMinutes)
        assertNull(allocation?.maximumMinutes)
    }

    @Test
    fun omittedFirmFlagsUseRustDefaultsAndWrongJsonTypesStayRejected() {
        val event = canonicalItem(eventDraft()).copy(
            flexibleConstraintsJson = """
                {
                  "dayweave_firm_block":{
                    "owned":true,
                    "starts_at":"2026-08-30T10:00:00Z",
                    "ends_at":"2026-08-30T11:00:00Z"
                  }
                }
            """.trimIndent(),
        ).toCanonicalDraft().eventTiming
        assertEquals(false, event?.allDay)
        assertEquals(false, event?.tentative)
        assertEquals(true, event?.busy)

        val emptyEvent = canonicalItem(
            CanonicalItemDraft(
                kind = ItemKind.EVENT,
                title = "Unresolved event",
                timezoneName = "UTC",
            ),
        ).copy(
            flexibleConstraintsJson =
                """{"calendar_event":null,"calendar_context":null,"dayweave_firm_block":null}""",
        ).toCanonicalDraft()
        assertNull(emptyEvent.eventTiming)

        listOf(
            """{"tags":null}""",
            """{"goal_ids":{}}""",
            """{"minimum_gap_minutes":null}""",
            """{"has_own_effort":null}""",
            """{"constraints":null}""",
            """{"constraints":{"preferred_daily_windows":null}}""",
            // Raw key presence is kind-significant even when an Option value is null.
            """{"habit_target":null}""",
        ).forEach { raw ->
            assertThrows(IllegalArgumentException::class.java) {
                canonicalItem(taskDraft()).copy(flexibleConstraintsJson = raw).toCanonicalDraft()
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            canonicalItem(eventDraft()).copy(
                flexibleConstraintsJson = """
                    {"dayweave_firm_block":{
                      "owned":true,
                      "starts_at":"2026-08-30T10:00:00Z",
                      "ends_at":"2026-08-30T11:00:00Z",
                      "all_day":null
                    }}
                """.trimIndent(),
            ).toCanonicalDraft()
        }
    }

    @Test
    fun schedulingOffsetsShareTheRustSafeMaximumWhileWorkCapsRemainU32() {
        val hard = CanonicalConstraintStrengthDraft.hard()
        CanonicalRecurrenceDraft(
            CanonicalRecurrenceKind.FREQUENCY,
            occurrencesPerPeriod = 1,
            period = CanonicalRecurrencePeriod.DAY,
            semantics = CanonicalRecurrenceSemantics.CALENDAR,
            minimumSpacingMinutes = MAX_SCHEDULING_OFFSET_MINUTES,
        ).requireValid()
        CanonicalQualifiedMinutesDraft(MAX_SCHEDULING_OFFSET_MINUTES, hard).requireValid(
            "Minimum notice",
            maximum = MAX_SCHEDULING_OFFSET_MINUTES,
        )
        CanonicalDependencyDraft(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            CanonicalDependencyRelation.FINISH_TO_START,
            MAX_SCHEDULING_OFFSET_MINUTES,
            hard,
        ).requireValid()
        CanonicalBufferPolicyDraft(MAX_SCHEDULING_OFFSET_MINUTES, 0, null).requireValid()
        CanonicalFlexibleConstraintsDraft(
            minimumGapMinutes = MAX_SCHEDULING_OFFSET_MINUTES,
        ).requireValid()
        CanonicalQualifiedMinutesDraft(4_294_967_295L, hard).requireValid(
            "Maximum daily work",
        )

        assertThrows(IllegalArgumentException::class.java) {
            CanonicalRecurrenceDraft(
                CanonicalRecurrenceKind.FREQUENCY,
                occurrencesPerPeriod = 1,
                period = CanonicalRecurrencePeriod.DAY,
                semantics = CanonicalRecurrenceSemantics.CALENDAR,
                minimumSpacingMinutes = MAX_SCHEDULING_OFFSET_MINUTES + 1,
            ).requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            CanonicalDependencyDraft(
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                CanonicalDependencyRelation.FINISH_TO_START,
                MAX_SCHEDULING_OFFSET_MINUTES + 1,
                hard,
            ).requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            CanonicalSchedulingConstraintsDraft(
                minimumNotice = CanonicalQualifiedMinutesDraft(
                    MAX_SCHEDULING_OFFSET_MINUTES + 1,
                    hard,
                ),
            ).requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            CanonicalBufferPolicyDraft(MAX_SCHEDULING_OFFSET_MINUTES + 1, 0, null).requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            CanonicalFlexibleConstraintsDraft(
                minimumGapMinutes = MAX_SCHEDULING_OFFSET_MINUTES + 1,
            ).requireValid()
        }
    }

    @Test
    fun eventDurationIsNullForFractionalBoundsAndNonNullMustBeExactSeconds() {
        val fractional = eventDraft().copy(
            durationSeconds = null,
            earliestStartAt = null,
            deadlineAt = null,
            eventTiming = CanonicalEventTimingDraft(
                "2026-08-30T10:00:00.000001Z",
                "2026-08-30T11:00:00.000002Z",
            ),
        )
        fractional.requireValid(ITEM_ID)
        assertThrows(IllegalArgumentException::class.java) {
            fractional.copy(durationSeconds = 3_600).requireValid(ITEM_ID)
        }
    }

    @Test
    fun incompleteInboxEventRetainsGenericMetadataAndOwnedTimingRequiresExplicitClearing() {
        val inbox = CanonicalItemDraft(
            kind = ItemKind.EVENT,
            title = "Unresolved event",
            timezoneName = "UTC",
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.LOW,
                tags = listOf("calendar candidate"),
                scheduling = CanonicalSchedulingConstraintsDraft(
                    minimumNotice = CanonicalQualifiedMinutesDraft(
                        15,
                        CanonicalConstraintStrengthDraft.hard(),
                    ),
                    includesNullOccurrenceWindow = true,
                ),
                hasOwnEffort = false,
            ),
        )

        inbox.requireValid(ITEM_ID)
        assertEquals(inbox, canonicalItem(inbox).toCanonicalDraft())
        assertThrows(IllegalArgumentException::class.java) {
            inbox.copy(
                placement = CanonicalDraftPlacement.PLANNED,
                durationSeconds = 3_600,
                earliestStartAt = "2026-08-30T10:00:00Z",
                deadlineAt = "2026-08-30T11:00:00Z",
                eventTiming = CanonicalEventTimingDraft(
                    "2026-08-30T10:00:00Z",
                    "2026-08-30T11:00:00Z",
                ),
            ).requireValid(ITEM_ID)
        }
    }

    @Test
    fun eventMetadataIntervalMayExceedClientDurationLimitWhenDurationIsAbsent() {
        eventDraft().copy(
            durationSeconds = null,
            earliestStartAt = null,
            deadlineAt = null,
            eventTiming = CanonicalEventTimingDraft(
                "2026-01-01T00:00:00Z",
                "2028-01-01T00:00:00Z",
            ),
        ).requireValid(ITEM_ID)
    }

    @Test
    fun durableReplacementJournalRequiresACompleteTypedBaseRoundTrip() {
        val supported = canonicalItem(taskDraft())
        val provider = supported.copy(
            kind = "event",
            recurrenceJson = null,
            durationSeconds = 3_600,
            earliestStartAt = "2026-09-03T08:00:00Z",
            deadlineAt = "2026-09-03T09:00:00Z",
            flexibleConstraintsJson = """
                {"calendar_event":{
                  "start":"2026-09-03T08:00:00Z",
                  "end":"2026-09-03T09:00:00Z",
                  "immutable":true,
                  "all_day":false,
                  "source_calendar_id":null
                }}
            """.trimIndent(),
            splitPolicyJson = """{"type":"indivisible"}""",
        )
        val unsupported = listOf(
            supported.copy(
                recurrenceJson = """{"type":"custom","rrule":"FREQ=DAILY"}""",
            ),
            provider,
            supported.copy(flexibleConstraintsJson = """{"future_metadata":true}"""),
            supported.copy(
                flexibleConstraintsJson =
                    """{"goal_ids":["aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"]}""",
            ),
            supported.copy(status = "scheduled"),
        )

        PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = supported.id,
            operation = CanonicalAuthoringOperation.REPLACE,
            draft = taskDraft(),
            expectedRevision = supported.revision,
            baseItem = supported,
            createdAt = "2026-09-03T07:00:00Z",
        ).requireValid()

        unsupported.forEach { base ->
            val replacement = PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = base.id,
                operation = CanonicalAuthoringOperation.REPLACE,
                draft = taskDraft(),
                expectedRevision = base.revision,
                baseItem = base,
                createdAt = "2026-09-03T07:00:00Z",
            )
            assertThrows(IllegalArgumentException::class.java) {
                requireCanonicalAuthoringJournalBudget(listOf(replacement))
            }

            val trash = PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = base.id,
                operation = CanonicalAuthoringOperation.TRASH,
                expectedRevision = base.revision,
                baseItem = base,
                createdAt = "2026-09-03T07:00:00Z",
            )
            requireCanonicalAuthoringJournalBudget(listOf(trash))
        }
    }

    @Test
    fun preferredStartRequiresDurationAndMustFinishWithinTheDay() {
        val preferred = CanonicalFlexibleConstraintsDraft(preferredStartMinute = 23 * 60)
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(durationSeconds = null, constraints = preferred)
                .requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(durationSeconds = 3_601, constraints = preferred)
                .requireValid(ITEM_ID)
        }
        taskDraft().copy(
            durationSeconds = 3_601,
            constraints = preferred.copy(preferredStartMinute = 22 * 60 + 59),
            split = CanonicalSplitDraft(),
        ).requireValid(ITEM_ID)
    }

    @Test
    fun allDayFirmBlockUsesExclusiveLocalMidnightsAndSoleMetadata() {
        val timing = CanonicalEventTimingDraft(
            startsAt = "2026-08-29T22:00:00Z",
            endsAt = "2026-08-30T22:00:00Z",
            allDay = true,
        )
        val draft = eventDraft().copy(
            durationSeconds = 24 * 60 * 60,
            earliestStartAt = timing.startsAt,
            deadlineAt = timing.endsAt,
            eventTiming = timing,
        )
        draft.requireValid(ITEM_ID)
        assertEquals(
            setOf("dayweave_firm_block"),
            draft.constraints.toCanonicalJson(
                timing,
                draft.durationSeconds,
                draft.timezoneName,
            ).keys,
        )
        assertThrows(IllegalArgumentException::class.java) {
            draft.copy(
                eventTiming = timing.copy(startsAt = "2026-08-29T23:00:00Z"),
                earliestStartAt = "2026-08-29T23:00:00Z",
                durationSeconds = 23 * 60 * 60,
            ).requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            draft.copy(
                constraints = CanonicalFlexibleConstraintsDraft(energy = EnergyLevel.DEEP),
            ).requireValid(ITEM_ID)
        }
    }

    @Test
    fun authoringRejectsJavaOnlyZonesAndNanosecondTimestamps() {
        listOf("+02:00", "GMT+02:00", "SystemV/EST5EDT").forEach { timezone ->
            assertThrows(IllegalArgumentException::class.java) {
                taskDraft().copy(timezoneName = timezone).requireValid(ITEM_ID)
            }
        }
        assertThrows(IllegalArgumentException::class.java) {
            taskDraft().copy(earliestStartAt = "2026-08-30T09:00:00.000000001Z")
                .requireValid(ITEM_ID)
        }
        assertThrows(IllegalArgumentException::class.java) {
            PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = ITEM_ID,
                operation = CanonicalAuthoringOperation.CREATE,
                draft = taskDraft(),
                createdAt = "2026-08-30T10:00:00.000000001Z",
            ).requireValid()
        }
        taskDraft().copy(timezoneName = "GMT").requireValid(ITEM_ID)
        taskDraft().copy(earliestStartAt = "2026-08-30T09:00:00.000001Z")
            .requireValid(ITEM_ID)
        taskDraft().copy(earliestStartAt = "2026-08-30T09:00:00+18:00")
            .requireValid(ITEM_ID)
        taskDraft().copy(
            earliestStartAt = "0001-01-01T00:00:00Z",
            deadlineAt = null,
        )
            .requireValid(ITEM_ID)
        taskDraft().copy(
            earliestStartAt = "9999-12-31T23:59:59.123456000-18:00",
            deadlineAt = null,
        )
            .requireValid(ITEM_ID)
        listOf(
            "2026-08-30T09:00Z",
            "2026-08-30T09:00:00+0200",
            "2026-08-30 09:00:00Z",
            "2026-08-30t09:00:00z",
            "2026-08-30T09:00:00+18:01",
            "2026-08-30T09:00:00+19:00",
            "2026-08-30T09:00:00.1234567Z",
            "2026-08-30T09:00:00.1234560000Z",
            "0000-08-30T09:00:00Z",
        ).forEach { value ->
            assertThrows(IllegalArgumentException::class.java) {
                taskDraft().copy(earliestStartAt = value).requireValid(ITEM_ID)
            }
        }
    }

    @Test
    fun authoringJournalHasPerMutationAndAggregateEncodedBudgets() {
        val oversizedBase = canonicalItem(taskDraft()).copy(
            flexibleConstraintsJson = "x".repeat(
                CanonicalAuthoringJournalPolicy.MAX_MUTATION_BYTES + 1,
            ),
        )
        assertThrows(IllegalArgumentException::class.java) {
            PendingCanonicalAuthoringMutation(
                id = MUTATION_ID,
                itemId = ITEM_ID,
                operation = CanonicalAuthoringOperation.TRASH,
                expectedRevision = oversizedBase.revision,
                baseItem = oversizedBase,
                createdAt = "2026-08-30T10:00:00Z",
            ).requireValid()
        }

        val largeDraft = taskDraft().copy(notes = "😀".repeat(100_000))
        val mutations = (0 until 12).map { index ->
            PendingCanonicalAuthoringMutation(
                id = stableUuid("large-mutation-$index"),
                itemId = stableUuid("large-item-$index"),
                operation = CanonicalAuthoringOperation.CREATE,
                draft = largeDraft,
                createdAt = "2026-08-30T10:00:00Z",
            )
        }
        mutations.forEach(PendingCanonicalAuthoringMutation::requireValid)
        assertThrows(IllegalArgumentException::class.java) {
            requireCanonicalAuthoringJournalBudget(mutations)
        }
    }

    @Test
    fun draftReconstructsAndMatchesTheExactCanonicalAuthoringSubset() {
        val draft = taskDraft().copy(
            isSensitive = true,
            constraints = CanonicalFlexibleConstraintsDraft(
                energy = EnergyLevel.DEEP,
                tags = listOf("work", "focus"),
                preferredStartMinute = 540,
                minimumGapMinutes = 30,
                maximumSessions = 3,
            ).normalized(),
        )
        val item = canonicalItem(draft)

        assertTrue(draft.matches(item))
        assertEquals(draft, item.toCanonicalDraft())
        assertFalse(draft.matches(item.copy(urgency = draft.urgency + 1)))
    }

    @Test
    fun submittedJournalRequiresExactIdentityBindingAndImmutableShape() {
        val draft = taskDraft()
        val mutation = PendingCanonicalAuthoringMutation(
            id = MUTATION_ID,
            itemId = ITEM_ID,
            operation = CanonicalAuthoringOperation.CREATE,
            draft = draft,
            createdAt = "2026-08-30T10:00:00Z",
            syncOrigin = "https://api.example.test/",
            configurationId = "connection-1",
            submittedAt = "2026-08-30T10:01:00Z",
        )

        mutation.requireValid()
        assertTrue(mutation.isSubmitted)
        assertThrows(IllegalArgumentException::class.java) {
            mutation.copy(idempotencyKey = "different").requireValid()
        }
        assertThrows(IllegalArgumentException::class.java) {
            mutation.copy(syncOrigin = null, submittedAt = null).requireValid()
        }
        mutation.copy(
            disposition = CanonicalAuthoringDisposition.CONFLICTED,
            diagnostic = "revision changed",
        ).requireValid()
    }

    private fun taskDraft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.INBOX,
        kind = ItemKind.TASK,
        title = "Write Android persistence tests",
        notes = "Keep the exact draft encrypted",
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        earliestStartAt = "2026-08-30T09:00:00Z",
        deadlineAt = "2026-08-31T09:00:00Z",
        split = CanonicalSplitDraft(
            kind = CanonicalSplitKind.SPLITTABLE,
            minimumChunkSeconds = 900,
            maximumChunkSeconds = 2_700,
        ),
        importance = 80,
        urgency = 60,
        siblingOrder = 2,
    )

    private fun fixtureItem(name: String, valid: Boolean = true): CanonicalItemSnapshot {
        val root = fixtureJson.parseToJsonElement(
            schedulingMetadataFixture(valid).readText(),
        ).jsonObject
        val fixture = root.getValue("cases").jsonArray
            .map { it.jsonObject }
            .single { it.getValue("name").jsonPrimitive.content == name }
        val fields = fixture.getValue("fields").jsonObject
        fun nullableString(key: String) = fields[key]
            ?.takeUnless { it == JsonNull }
            ?.jsonPrimitive
            ?.contentOrNull
        return CanonicalItemSnapshot(
            id = fields.getValue("item_id").jsonPrimitive.content,
            isSensitive = false,
            kind = fields.getValue("kind").jsonPrimitive.content,
            status = fields.getValue("status").jsonPrimitive.content,
            title = name.replace('_', ' '),
            notes = null,
            timezoneName = fields.getValue("timezone_name").jsonPrimitive.content,
            durationSeconds = fields["duration_seconds"]?.jsonPrimitive?.longOrNull,
            deadlineAt = nullableString("deadline_at"),
            earliestStartAt = nullableString("earliest_start_at"),
            recurrenceJson = fields["recurrence"]?.takeUnless { it == JsonNull }?.toString(),
            flexibleConstraintsJson = fields.getValue("flexible_constraints").toString(),
            splitPolicyJson = fields.getValue("split_policy").toString(),
            importance = 50,
            urgency = 50,
            parentId = nullableString("parent_id"),
            siblingOrder = 0,
            isExecutable = true,
            revision = 1,
            createdAt = "2026-09-03T06:00:00Z",
            updatedAt = "2026-09-03T06:00:00Z",
        )
    }

    private fun fixtureCaseNames(valid: Boolean): Set<String> = fixtureJson.parseToJsonElement(
        schedulingMetadataFixture(valid).readText(),
    ).jsonObject.getValue("cases").jsonArray.mapTo(linkedSetOf()) {
        it.jsonObject.getValue("name").jsonPrimitive.content
    }

    private fun schedulingMetadataFixture(valid: Boolean): File {
        val fileName = if (valid) "valid-rich-items.json" else "invalid-items.json"
        val relative = "fixtures/scheduling-metadata/$fileName"
        return generateSequence(File(requireNotNull(System.getProperty("user.dir")))) {
            it.parentFile
        }
            .map { File(it, relative) }
            .firstOrNull(File::isFile)
            ?: error("Unable to locate $relative")
    }

    private fun eventDraft() = CanonicalItemDraft(
        placement = CanonicalDraftPlacement.PLANNED,
        kind = ItemKind.EVENT,
        title = "Planning call",
        timezoneName = "Europe/Madrid",
        durationSeconds = 3_600,
        earliestStartAt = "2026-08-30T10:00:00Z",
        deadlineAt = "2026-08-30T11:00:00Z",
        eventTiming = CanonicalEventTimingDraft(
            startsAt = "2026-08-30T10:00:00Z",
            endsAt = "2026-08-30T11:00:00Z",
        ),
    )

    private fun canonicalItem(draft: CanonicalItemDraft) = CanonicalItemSnapshot(
        id = ITEM_ID,
        isSensitive = draft.isSensitive,
        kind = draft.kind.name.lowercase(),
        status = draft.placement.wireValue,
        title = draft.title,
        notes = draft.notes,
        timezoneName = draft.timezoneName,
        durationSeconds = draft.durationSeconds,
        deadlineAt = draft.deadlineAt,
        earliestStartAt = draft.earliestStartAt,
        recurrenceJson = draft.recurrence?.toCanonicalJson()?.toString(),
        flexibleConstraintsJson = draft.constraints.toCanonicalJson(
            draft.eventTiming,
            draft.durationSeconds,
            draft.timezoneName,
        ).toString(),
        splitPolicyJson = draft.split.toCanonicalJson(draft.durationSeconds).toString(),
        importance = draft.importance,
        urgency = draft.urgency,
        parentId = draft.parentId,
        siblingOrder = draft.siblingOrder,
        isExecutable = true,
        revision = 1,
        createdAt = "2026-08-30T10:00:00Z",
        updatedAt = "2026-08-30T10:00:00Z",
    )

    private fun stableUuid(seed: String): String =
        UUID.nameUUIDFromBytes(seed.toByteArray()).toString()

    private companion object {
        const val ITEM_ID = "11111111-1111-4111-8111-111111111111"
        const val MUTATION_ID = "22222222-2222-4222-8222-222222222222"
    }
}
