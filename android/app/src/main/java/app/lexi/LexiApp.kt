package app.lexi

import android.app.Application

class LexiApp : Application() {
    override fun onCreate() {
        super.onCreate()
        NativeBridge.init(this, CallModel)
        AudioRouter.init(this)
    }
}
