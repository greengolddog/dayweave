package com.greengolddog.dayweave.model

/**
 * Rust's `str::chars` limits Unicode scalar values, while Kotlin's `String.length` counts UTF-16
 * code units. Count valid surrogate pairs once and fail closed on an unpaired surrogate so Android
 * accepts exactly the same user-visible text lengths as the habit service.
 */
internal fun String.hasAtMostUnicodeScalars(maximum: Int): Boolean {
    require(maximum >= 0)
    var index = 0
    var count = 0
    while (index < length) {
        if (++count > maximum) return false
        val character = this[index]
        when {
            Character.isHighSurrogate(character) -> {
                if (
                    index + 1 >= length ||
                    !Character.isLowSurrogate(this[index + 1])
                ) {
                    return false
                }
                index += 2
            }
            Character.isLowSurrogate(character) -> return false
            else -> index += 1
        }
    }
    return true
}
