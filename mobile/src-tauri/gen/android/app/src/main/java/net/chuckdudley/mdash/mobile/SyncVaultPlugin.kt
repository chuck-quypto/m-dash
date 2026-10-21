package net.chuckdudley.mdash.mobile

import android.app.Activity
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.system.Os
import android.system.OsConstants
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Plugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class VaultArgs { var op: String = ""; var contents: String? = null }

@TauriPlugin
class SyncVaultPlugin(private val activity: Activity): Plugin(activity) {
    private external fun initTls(context: android.content.Context)
    init { initTls(activity.applicationContext) }
    private val alias = "m-dash-mobile-sync-v1"
    private fun key(create: Boolean): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        val existing = store.getKey(alias, null) as? SecretKey
        if (existing != null) return existing
        check(create) { "Missing key" }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
    }
    private fun syncDirectory() {
        val fd = Os.open(activity.noBackupFilesDir.path, OsConstants.O_RDONLY, 0)
        try { Os.fsync(fd) } finally { Os.close(fd) }
    }
    @Command
    fun vault(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(VaultArgs::class.java)
            val file = File(activity.noBackupFilesDir, "mobile-sync-v1.enc")
            val result = JSObject()
            when (args.op) {
                "read" -> {
                    if (!file.exists()) result.put("contents", org.json.JSONObject.NULL)
                    else {
                        val bytes = file.inputStream().use { check(it.channel.size() in 28..150000); it.readBytes() }
                        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                        cipher.init(Cipher.DECRYPT_MODE, key(false), GCMParameterSpec(128, bytes.copyOfRange(0, 12)))
                        cipher.updateAAD(alias.toByteArray(Charsets.UTF_8))
                        result.put("contents", cipher.doFinal(bytes.copyOfRange(12, bytes.size)).toString(Charsets.UTF_8))
                    }
                }
                "write" -> {
                    val plain = requireNotNull(args.contents).toByteArray(Charsets.UTF_8)
                    check(plain.size <= 140000)
                    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                    cipher.init(Cipher.ENCRYPT_MODE, key(true)); cipher.updateAAD(alias.toByteArray(Charsets.UTF_8))
                    val bytes = cipher.iv + cipher.doFinal(plain)
                    val pending = File(activity.noBackupFilesDir, "mobile-sync-v1.next")
                    try {
                        FileOutputStream(pending).use { it.write(bytes); it.fd.sync() }
                        Files.move(pending.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
                        syncDirectory()
                    } finally { pending.delete() }
                }
                "clear" -> { Files.deleteIfExists(file.toPath()); syncDirectory(); KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(alias) } }
                else -> error("Unknown operation")
            }
            invoke.resolve(result)
        } catch (_: Exception) { invoke.reject("Protected sync storage unavailable. The operation could not be confirmed; unlock the phone and retry.") }
    }
}
