# El spot

El vídeo de treinta segundos que explica qué es MultiDrive. Sale en la portada, en las dos
lenguas, y va incrustado en los dos README del repositorio.

## El fichero

`multidrive-spot.mp4`, y se publica con ese nombre a propósito. El original se llamaba
`multidrive-spot-v4-16x9.mp4`: la versión en el nombre sirve mientras se está montando, pero
publicada deja una URL que o miente cuando llegue la v5, o hay que cambiarla y entonces se
rompen los enlaces que alguien haya repartido. La versión se anota aquí, no en la URL.

Publicado hoy: **v4**.

Medido del propio fichero, no supuesto:

| | |
|---|---|
| Duración | 30,00 s exactos |
| Resolución | 1920×1080 (16:9) |
| Códec | H.264 (`avc1`), contenedor `isom`/`mp41` |
| Pistas | **una sola, de vídeo. No tiene audio** |
| Tamaño | 1,75 MB |
| `moov` antes de `mdat` | sí |

Esos dos últimos son los que importan para la página.

**No tiene audio**, así que lo que explica lo explica con texto en pantalla. De ahí vienen dos
cosas: que no haga falta subtitular nada (no hay nada que oír), y que **no se reproduzca
sola**. Un texto que se lee en pantalla necesita el ritmo de quien lo lee, y una explicación
de 30 segundos en bucle cansa a la segunda vuelta.

**`moov` va antes de `mdat`**, que es lo que se llama *faststart*: el índice del fichero está
al principio, así que el navegador puede empezar a pintar sin bajárselo entero. Si alguna vez
se rehace el vídeo con otra herramienta y sale al revés, el reproductor se queda en blanco
hasta tener el fichero completo. Se comprueba así:

```sh
grep -abo -E "moov|mdat" site/video/multidrive-spot.mp4 | head -2
```

Tiene que salir `moov` con un desplazamiento menor que el de `mdat`. Al revés, hay que volver
a empaquetarlo
(`ffmpeg -i entrada.mp4 -c copy -movflags +faststart salida.mp4`).

## No lleva cartel

La etiqueta `<video>` de la portada no tiene `poster`, así que el navegador pinta el primer
fotograma. Funciona, pero un cartel propio se vería mejor y además se controlaría qué se ve
antes de pulsar. Para ponerlo hacen falta dos cosas que hoy no están: el fotograma exportado
como `.png` o `.webp`, y acordarse de publicarlo —otro fichero más del que depende que la
página se vea bien—.

En GitHub da igual: su sanitizador **borra el atributo `poster`** (y `playsinline`), así que
ahí el reproductor sale en negro hasta que alguien le da al play, y eso no tiene arreglo por
nuestra parte.

## Al cambiarlo

1. Sustituir el fichero con **el mismo nombre**, para no romper enlaces ni los dos README.
2. Anotar arriba la versión nueva.
3. Comprobar el *faststart* con la orden de más arriba.
4. Publicarlo: **no va en el despliegue del HTML**, igual que los logotipos. Ver
   [DESPLIEGUE.md](../../docs/DESPLIEGUE.md#el-spot).
