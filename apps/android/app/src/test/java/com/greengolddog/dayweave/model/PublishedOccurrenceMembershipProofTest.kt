package com.greengolddog.dayweave.model

import kotlinx.serialization.SerializationException
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.Json
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class PublishedOccurrenceMembershipProofTest {
    @Test
    fun proofRequiresCanonicalUniqueSortedBoundedExactStateMembership() {
        val first = member("11111111-1111-5111-8111-111111111111")
        val second = member("22222222-2222-5222-8222-222222222222")
        val valid = proof(listOf(first, second))

        assertTrue(valid.hasValidShape())
        assertFalse(proof(listOf(first, first)).hasValidShape())
        assertFalse(proof(listOf(second, first)).hasValidShape())
        assertFalse(
            proof(
                List(PublishedOccurrenceMembershipProofSnapshot.MAX_OCCURRENCES + 1) { first },
            ).hasValidShape(),
        )
        assertFalse(
            proof(
                listOf(first.copy(plannerOccurrenceId = "11111111-1111-4111-8111-111111111111")),
            ).hasValidShape(),
        )
    }

    @Test
    fun unknownOrStructurallyDifferentPersistedStateFailsStrictDecoding() {
        assertThrows(SerializationException::class.java) {
            Json.decodeFromString<PublishedOccurrenceMembershipSnapshot>(
                """{"plannerOccurrenceId":"11111111-1111-5111-8111-111111111111","seriesItemId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","state":"future_state"}""",
            )
        }
        assertThrows(SerializationException::class.java) {
            Json.decodeFromString<PublishedOccurrenceMembershipSnapshot>(
                """{"plannerOccurrenceId":"11111111-1111-5111-8111-111111111111","seriesItemId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","state":"generated","injected":true}""",
            )
        }
    }

    private fun member(plannerOccurrenceId: String) = PublishedOccurrenceMembershipSnapshot(
        plannerOccurrenceId = plannerOccurrenceId,
        seriesItemId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        state = PublishedOccurrenceStateSnapshot.GENERATED,
    )

    private fun proof(
        occurrences: List<PublishedOccurrenceMembershipSnapshot>,
    ) = PublishedOccurrenceMembershipProofSnapshot(
        schemaVersion = PublishedOccurrenceMembershipProofSnapshot.CURRENT_SCHEMA_VERSION,
        syncOrigin = "https://api.example.test/",
        configurationId = "connection-1",
        revision = PublishedScheduleRevisionSnapshot(
            id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            revisionNumber = 7uL,
            revision = "7:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            inputDigest = "sha256:${"a".repeat(64)}",
            horizonStart = "2026-09-01T00:00:00Z",
            horizonEnd = "2026-09-03T00:00:00Z",
            timezoneName = "UTC",
            publishedAt = "2026-09-01T00:00:00Z",
        ),
        occurrences = occurrences,
    )
}
