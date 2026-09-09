package com.greengolddog.dayweave.network

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.conflate

/** Foreground-scoped availability hints only: never proof that the server/account is reachable. */
internal fun foregroundNetworkReconnects(context: Context): Flow<Unit> = callbackFlow {
    val connectivity = context.getSystemService(ConnectivityManager::class.java)
    val hints = ForegroundNetworkReconnectHints<Network>()
    val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) { if (hints.available(network)) trySend(Unit) }
        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
            if (hints.validated(network, capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED))) trySend(Unit)
        }
        override fun onLost(network: Network) { hints.lost(network) }
    }
    connectivity.registerDefaultNetworkCallback(callback)
    awaitClose { connectivity.unregisterNetworkCallback(callback) }
}.conflate()

/** An Internet recovery can change validation without replacing the connected Wi-Fi network. */
internal class ForegroundNetworkReconnectHints<N : Any> {
    private var active: N? = null
    private var wasValidated = false
    @Synchronized fun available(network: N): Boolean {
        if (active == network) return false
        active = network
        wasValidated = false
        return true
    }
    @Synchronized fun validated(network: N, value: Boolean): Boolean {
        if (active != network) return false
        val regained = value && !wasValidated
        wasValidated = value
        return regained
    }
    @Synchronized fun lost(network: N) {
        if (active == network) { active = null; wasValidated = false }
    }
}
