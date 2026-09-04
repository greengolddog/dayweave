package com.greengolddog.dayweave.sync

/**
 * One proposal or direct aggregate mutation can emit at most one direct row
 * plus two derived parent refreshes for each of the 100 bounded commands.
 * The server may therefore expand a requested delta page through the end of
 * the boundary group instead of exposing a partially applied item graph.
 */
internal const val MAX_ATOMIC_ITEM_CHANGE_GROUP_SIZE = 300

internal fun maximumItemDeltaResponseChanges(requestedLimit: Int): Int {
    require(requestedLimit > 0)
    return Math.addExact(requestedLimit - 1, MAX_ATOMIC_ITEM_CHANGE_GROUP_SIZE)
}
