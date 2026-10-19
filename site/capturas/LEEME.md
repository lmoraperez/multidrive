# Las capturas del móvil

Cuatro pantallas de la aplicación de Android. Se usan en la portada (`/#capturas` y
`/en/#screenshots`), en los dos README, y son las mismas que van a la ficha de Google Play.

## De dónde salen

De un teléfono de verdad, con la versión que hay publicada corriendo encima. **Sin maquetar,
sin marco de móvil dibujado y sin retocar.** Eso es a propósito: una captura compuesta enseña
lo que uno quiere que se vea, y lo que hace falta enseñar aquí es lo que hay.

| | |
|---|---|
| Aparato | TECNO LH7n |
| Android | 14 |
| Tamaño | 1080 × 2460 |
| Tomadas | 19 de octubre de 2026 |

Se ve la barra de estado del teléfono, con su hora y su batería. Se deja: recortarla sería
retocar, y no estorba.

## Cuáles son

| Fichero | Qué sale |
|---|---|
| `principal.png` | El volumen abierto, con las dos cuentas y el espacio de cada una |
| `topologia.png` | La elección de reparto, con lo que cuesta cada opción |
| `archivos.png` | El explorador, con tres carpetas |
| `ampliar.png` | La pantalla de meterle cuentas nuevas al volumen |

El volumen de `principal.png` es `espejo:2` sobre dos cuentas de Google Drive de prueba. Los
números de espacio son los de esas cuentas, no inventados.

## Cuándo hay que rehacerlas

Cuando la aplicación deje de parecerse a esto. En concreto: si cambia el distintivo de etapa
de la cabecera, si se mueven los botones principales, o si se añade un proveedor nuevo a las
pantallas de conectar. Una captura vieja es una promesa que la aplicación ya no cumple.

Se toman así, con el teléfono conectado:

```
adb exec-out screencap -p > principal.png
```

## Para Google Play

Play no acepta estas tal cual: pide que el lado largo no pase del doble del corto, y
2460 / 1080 son 2,28. Las de la ficha se generan con bordes a los lados, sin recortar nada,
con `packaging/android/capturas-para-play.ps1`.
