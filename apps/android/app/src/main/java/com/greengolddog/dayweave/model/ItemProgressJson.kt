package com.greengolddog.dayweave.model

/** Runs before a JSON tree can erase duplicate keys or normalize integer tokens. */
internal fun requireStrictItemProgressJson(source: String, integersOnly: Boolean = true, maxDepth: Int = 32) =
    ItemProgressJsonScanner(source, integersOnly, maxDepth).validate()

private class ItemProgressJsonScanner(private val source: String, private val integersOnly: Boolean, private val maxDepth: Int) {
    private var index = 0
    fun validate() { value(0); whitespace(); require(index == source.length) }

    private fun value(depth: Int) {
        require(depth <= maxDepth)
        whitespace()
        when (source.getOrNull(index)) {
            '{' -> {
                index++
                whitespace()
                if (take('}')) return
                val keys = hashSetOf<String>()
                do {
                    whitespace()
                    require(keys.add(string()))
                    whitespace()
                    require(take(':'))
                    value(depth + 1)
                    whitespace()
                    if (take('}')) return
                } while (take(','))
                throw IllegalArgumentException("Invalid progress JSON object")
            }
            '[' -> {
                index++
                whitespace()
                if (take(']')) return
                do {
                    value(depth + 1)
                    whitespace()
                    if (take(']')) return
                } while (take(','))
                throw IllegalArgumentException("Invalid progress JSON array")
            }
            '"' -> string()
            else -> {
                val start = index
                while (index < source.length && source[index] !in " \t\r\n,]}") index++
                val token = source.substring(start, index)
                require(token in setOf("null", "true", "false") || (if (integersOnly) INTEGER else NUMBER).matches(token))
            }
        }
    }

    private fun string(): String {
        val start = index
        require(take('"'))
        while (index < source.length) {
            when (source[index++]) {
                '"' -> return ITEM_PROGRESS_JSON.decodeFromString(source.substring(start, index))
                '\\' -> {
                    require(index < source.length)
                    if (source[index++] == 'u') repeat(4) {
                        require(source.getOrNull(index)?.let { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' } == true)
                        index++
                    }
                }
            }
        }
        throw IllegalArgumentException("Invalid progress JSON string")
    }

    private fun take(char: Char): Boolean = if (source.getOrNull(index) == char) { index++; true } else false
    private fun whitespace() { while (index < source.length && source[index] in " \t\r\n") index++ }
    private companion object {
        val INTEGER = Regex("(?:0|[1-9][0-9]*|-[1-9][0-9]*)")
        val NUMBER = Regex("-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?(?:[eE][+-]?[0-9]+)?")
    }
}
