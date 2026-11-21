# JNI: Rust looks these up by name.
-keepclasseswithmembernames class * { native <methods>; }
-keep class app.lexi.NativeBridge { *; }
-keep interface app.lexi.NativeCallback { *; }
-keepclassmembers class * implements app.lexi.NativeCallback { public void onState(int, java.lang.String); }
