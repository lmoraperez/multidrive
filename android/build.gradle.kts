// Sin nada en la raiz: los plugins se declaran aqui y se aplican en el modulo. Es lo que
// recomienda Gradle desde hace versiones, y evita que la raiz arrastre configuracion que solo
// le hace falta a `:app`.
plugins {
    alias(libs.plugins.android.application) apply false
}
