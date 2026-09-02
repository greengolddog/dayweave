package com.greengolddog.dayweave.assistant

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AssistantModelsTest {
    @Test
    fun validSupplementaryUnicodeIsAcceptedButMalformedOrDirectionalTextIsRejected() {
        assertTrue("Plan around my run 🏃🏽‍♂️".isValidAssistantConversationText(8 * 1_024))
        assertTrue("Focus 😀".isValidAssistantContextText(160, allowEmpty = false))

        assertFalse("unpaired high \uD800".isValidAssistantConversationText(8 * 1_024))
        assertFalse("unpaired low \uDC00".isValidAssistantConversationText(8 * 1_024))
        assertFalse("spoofed\u202Etext".isValidAssistantConversationText(8 * 1_024))
        assertFalse("line\nbreak".isValidAssistantContextText(160, allowEmpty = false))
    }
}
