// Desde AGP 9.0 el plugin de Kotlin va dentro: declararlo aparte no es redundante, es un
// error de compilacion que dice «ya no hace falta».
plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "net.multidrive"
    compileSdk = 36

    defaultConfig {
        applicationId = "net.multidrive"
        // 26 y no menos: es donde aparece `StorageManager.openProxyFileDescriptor`, que es lo
        // unico que permite leer por rangos. Sin eso habria que bajar el fichero entero antes
        // de abrirlo, y entonces esto es un descargador con mas pasos. Ver docs/ANDROID.md.
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.0.1"
    }

    // El `.so` no lo construye Gradle: lo deja ahi `packaging/android/compilar.ps1` con cargo y
    // el NDK. Se hace asi, y no con `cargo-ndk` o `externalNativeBuild`, porque el nucleo se
    // compila igual para el escritorio y no tiene por que aprender a construirse de dos
    // maneras. Lo que Gradle hace es empaquetar lo que encuentre.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

}

// El componente Kotlin que exige `rustls-platform-verifier`: sin el, la primera peticion HTTPS
// entra en panico. El `.aar` lo trae el propio crate y lo copia aqui
// `packaging/android/compilar.ps1`, por lo mismo que el `.so`: la busqueda del lado de cargo se
// hace en el guion, y Gradle solo empaqueta lo que encuentra.
//
// La receta oficial del crate hace que Gradle ejecute `cargo metadata` para localizarlo. Se
// evita a proposito: ataria la compilacion de Android a tener cargo delante, que es justo la
// dependencia que este proyecto no quiere en los dos sentidos.
dependencies {
    implementation(files("libs/rustls-platform-verifier.aar"))
}
