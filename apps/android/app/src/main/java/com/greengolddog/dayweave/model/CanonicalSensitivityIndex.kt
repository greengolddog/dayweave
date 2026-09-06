package com.greengolddog.dayweave.model

/**
 * Batch equivalent of [effectiveCanonicalSensitivity]. A snapshot can retain multiple possible
 * parents during a queued move. Only nodes whose complete ancestry resolves are admitted;
 * missing parents, cycles and their descendants remain protected by the default lookup.
 */
internal class CanonicalSensitivityIndex private constructor(
    private val resolved: Map<String, Boolean>,
) {
    operator fun get(itemId: String): Boolean = resolved[itemId] ?: true

    companion object {
        fun build(
            items: List<CanonicalItemSnapshot>,
            pendingMutation: PendingCanonicalMutation? = null,
            pendingAuthoringMutations: List<PendingCanonicalAuthoringMutation> = emptyList(),
        ): CanonicalSensitivityIndex {
            val own = mutableMapOf<String, Boolean>()
            val parents = mutableMapOf<String, MutableSet<String>>()
            fun retain(item: CanonicalItemSnapshot) {
                own[item.id] = own[item.id] == true || item.isSensitive
                item.parentId?.let { parents.getOrPut(item.id) { mutableSetOf() }.add(it) }
            }
            items.forEach(::retain)
            for (mutation in pendingAuthoringMutations) {
                // Match the existing resolver: an invalid journal entry protects the entire view.
                if (runCatching { mutation.requireValid() }.isFailure) {
                    return CanonicalSensitivityIndex(emptyMap())
                }
                mutation.baseItem?.let(::retain)
                when (mutation.operation) {
                    CanonicalAuthoringOperation.CREATE,
                    CanonicalAuthoringOperation.REPLACE,
                    -> {
                        val draft = mutation.draft ?: return CanonicalSensitivityIndex(emptyMap())
                        own[mutation.itemId] = own[mutation.itemId] == true || draft.isSensitive
                        draft.parentId?.let {
                            parents.getOrPut(mutation.itemId) { mutableSetOf() }.add(it)
                        }
                    }
                    CanonicalAuthoringOperation.TRASH -> Unit
                    CanonicalAuthoringOperation.RESTORE -> if (mutation.baseItem == null) {
                        own[mutation.itemId] = true
                    }
                }
            }
            pendingMutation?.takeIf(PendingCanonicalMutation::targetIsSensitive)?.let {
                own[it.itemId] = true
            }

            val children = mutableMapOf<String, MutableList<String>>()
            val remaining = mutableMapOf<String, Int>()
            val inherited = own.toMutableMap()
            val pending = ArrayDeque<String>()
            own.keys.forEach { id ->
                val possibleParents = parents[id].orEmpty()
                remaining[id] = possibleParents.size
                if (possibleParents.isEmpty()) pending.addLast(id)
                possibleParents.forEach { parent ->
                    children.getOrPut(parent) { mutableListOf() }.add(id)
                }
            }
            val resolved = mutableMapOf<String, Boolean>()
            while (pending.isNotEmpty()) {
                val id = pending.removeFirst()
                val isSensitive = inherited.getValue(id)
                resolved[id] = isSensitive
                children[id].orEmpty().forEach { child ->
                    inherited[child] = inherited.getValue(child) || isSensitive
                    val count = remaining.getValue(child) - 1
                    remaining[child] = count
                    if (count == 0) pending.addLast(child)
                }
            }
            // Unprocessed nodes depend on missing or cyclic ancestry; lookup fails closed.
            return CanonicalSensitivityIndex(resolved)
        }
    }
}
