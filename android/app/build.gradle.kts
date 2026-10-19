// `java` dentro del bloque `android` resuelve a la extension de Gradle, no al paquete, asi
// que el nombre completo `java.util.Properties` no compila ahi. Se importa.
import java.util.Properties

// Desde AGP 9.0 el plugin de Kotlin va dentro: declararlo aparte no es redundante, es un
// error de compilacion que dice Â«ya no hace faltaÂ».
plugins {
    alias(libs.plugins.android.application)
}

android {
    // El paquete del **codigo**: de aqui salen `R`, los nombres de las clases del manifiesto y
    // los simbolos JNI (`Java_net_multidrive_nucleo_Nucleo_*`). No tiene por que coincidir con
    // `applicationId`, y aqui no coincide a proposito: cambiarlo obligaria a renombrar tambien
    // las funciones del lado de Rust, que es mucho movimiento para no ganar nada.
    namespace = "net.multidrive"
    compileSdk = 36

    defaultConfig {
        // La identidad de la aplicacion para Android y para Play. **No se puede cambiar una vez
        // publicada**: para Play, otro `applicationId` es otra aplicacion distinta.
        //
        // Es `com.jlsoltech.multidrive` y no `net.multidrive` porque asi se creo la ficha en
        // Play Console, y ahi el nombre de paquete se fija al crearla.
        applicationId = "com.jlsoltech.multidrive"
        // 26 y no menos: es donde aparece `StorageManager.openProxyFileDescriptor`, que es lo
        // unico que permite leer por rangos. Sin eso habria que bajar el fichero entero antes
        // de abrirlo, y entonces esto es un descargador con mas pasos. Ver docs/ANDROID.md.
        minSdk = 26
        targetSdk = 36
        // El contador de Play. Sube en **cada** envio, aunque no cambie nada visible: Play no
        // acepta dos veces el mismo. No es la version que lee nadie, es la que ordena.
        versionCode = 21
        versionName = "0.0.1"
    }

    // El `.so` no lo construye Gradle: lo deja ahi `packaging/android/compilar.ps1` con cargo y
    // el NDK. Se hace asi, y no con `cargo-ndk` o `externalNativeBuild`, porque el nucleo se
    // compila igual para el escritorio y no tiene por que aprender a construirse de dos
    // maneras. Lo que Gradle hace es empaquetar lo que encuentre.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")

    // Solo por `BuildConfig.APPLICATION_ID`: es lo que permite que la autoridad del proveedor
    // de documentos salga del mismo sitio en el manifiesto y en el codigo, en vez de estar
    // escrita a mano en los dos y poder discrepar.
    buildFeatures {
        buildConfig = true
    }

    // La clave con la que se firma lo que se reparte.
    //
    // Vive fuera del repositorio, en `packaging/android/`, junto a su contrasena. Quien tenga
    // ese almacen puede publicar algo que Android instale **encima** de MultiDrive sin avisar
    // a nadie, porque para el sistema seria la misma aplicacion.
    //
    // Si no esta, no se inventa nada: `release` se queda sin firma y la compilacion falla mas
    // abajo diciendo por que. Lo que no puede pasar es que salga un APK firmado con la clave de
    // depuracion --que es la misma para todo el mundo-- sin que nadie se entere.
    val firma = rootProject.file("../packaging/android/firma.conf")
    val almacen = rootProject.file("../packaging/android/multidrive.jks")
    val datos = Properties().apply {
        if (firma.exists()) firma.inputStream().use { load(it) }
    }

    signingConfigs {
        if (firma.exists() && almacen.exists()) {
            create("reparto") {
                storeFile = almacen
                storePassword = datos.getProperty("almacen")
                keyAlias = datos.getProperty("alias")
                keyPassword = datos.getProperty("clave")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // `debuggable` ya es falso aqui por defecto, y **es lo que importa de esta
            // variante**: en un APK depurable cualquiera con el telefono y un cable lee el
            // fichero de configuracion de la aplicacion, que es donde viven los tokens de
            // acceso a las nubes de quien la instale.
            signingConfig = signingConfigs.findByName("reparto")
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

// Un APK de reparto sin firmar no se instala, y descubrirlo cuando ya esta repartido es tarde.
// Se para aqui, con el motivo y con lo que hay que hacer.
tasks.matching { it.name == "assembleRelease" }.configureEach {
    doFirst {
        val firma = rootProject.file("../packaging/android/firma.conf")
        val almacen = rootProject.file("../packaging/android/multidrive.jks")
        if (!firma.exists() || !almacen.exists()) {
            throw GradleException(
                "Falta la clave de firma. Se esperan estos dos, y no estan en el repositorio " +
                    "a proposito:\n  ${almacen.path}\n  ${firma.path}\n\n" +
                    "Se generan con `packaging/android/firmar.ps1`. Si ya existian en otro " +
                    "equipo, traelos de la copia de seguridad: no se pueden volver a generar " +
                    "iguales, y con otra clave nadie puede actualizar sin desinstalar antes."
            )
        }
    }
}
