// El proyecto de Android vive aparte del workspace de Rust a proposito: son dos sistemas de
// construccion que no se entienden entre si, y mezclarlos obliga a quien solo quiere compilar
// el escritorio a tener instalado un SDK que no necesita.
//
// El puente entre los dos es `packaging/android/compilar.ps1`, que construye el `.so` con cargo
// y lo deja donde Gradle lo espera.

pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "MultiDrive"
include(":app")
