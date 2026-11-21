package app.lexi

import android.content.Context
import java.security.MessageDigest
import java.util.UUID

/** Random per-install UUID; only its SHA-256 hex ever leaves the device (SPEC N7). */
object InstallId {
    fun hash(context: Context): String {
        val prefs = context.getSharedPreferences("lexi", Context.MODE_PRIVATE)
        val id = prefs.getString("install_id", null)
            ?: UUID.randomUUID().toString().also { prefs.edit().putString("install_id", it).apply() }
        return MessageDigest.getInstance("SHA-256").digest(id.toByteArray())
            .joinToString("") { "%02x".format(it) }
    }
}
