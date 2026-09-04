package com.greengolddog.dayweave.model

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class UnicodeTextTest {
    @Test
    fun astralCharactersCountAsOneUnicodeScalar() {
        val tenThousandEmoji = "😀".repeat(10_000)

        assertTrue(tenThousandEmoji.length > 10_000)
        assertTrue(tenThousandEmoji.hasAtMostUnicodeScalars(10_000))
        assertFalse((tenThousandEmoji + "😀").hasAtMostUnicodeScalars(10_000))
    }

    @Test
    fun malformedSurrogatesAreNotAcceptedAsUnicodeScalars() {
        assertFalse("\uD83D".hasAtMostUnicodeScalars(1))
        assertFalse("\uDE00".hasAtMostUnicodeScalars(1))
    }

    @Test
    fun habitOutcomeUsesTheServerScalarLimitsForUnitAndNote() {
        val outcome = HabitOutcomeInputSnapshot(
            status = HabitOutcomeStatusSnapshot.COMPLETED,
            progressBasisPoints = 10_000,
            quantity = 1,
            unit = "💧".repeat(200),
            actualSeconds = null,
            note = "😀".repeat(10_000),
            occurredAt = "2026-09-01T07:30:00Z",
        )

        assertTrue(runCatching(outcome::requireValid).isSuccess)
        assertTrue(
            runCatching {
                outcome.copy(note = requireNotNull(outcome.note) + "😀").requireValid()
            }.isFailure,
        )
    }
}
