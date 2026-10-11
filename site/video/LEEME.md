# El spot

El vídeo de treinta segundos que explica qué es MultiDrive. Se reproduce en la portada, en las
dos lenguas. En los README del repositorio **no**, por lo que se cuenta más abajo: ahí va el
cartel, enlazado a la portada.

Aquí hay dos ficheros publicados, y los dos hacen falta: el vídeo y su cartel.

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

## El cartel

`cartel.png`, 1920×1080, lo que el `<video>` de la portada pone en `poster` y lo que se ve en
los dos README. Sale de `cartel.svg`, que es la fuente y **no se publica**.

**No es un fotograma del vídeo**: es una composición con la marca, y por eso no finge serlo.
Sacar un fotograma de verdad exige decodificar H.264, o sea ffmpeg, que no está en este
equipo. Si algún día se quiere el fotograma real, se saca así y se sustituye el `.png` sin
tocar nada más:

```sh
ffmpeg -ss 2 -i site/video/multidrive-spot.mp4 -frames:v 1 site/video/cartel.png
```

Se rasteriza desde el SVG igual que los iconos de la marca, con Edge sin ventana:

```powershell
$svg = Get-Content site\video\cartel.svg -Raw -Encoding UTF8
"<!doctype html><meta charset=`"utf-8`"><style>html,body{margin:0;background:#0d1017}svg{display:block}</style>$svg" |
  Out-File -Encoding utf8 $env:TEMP\cartel.html
Start-Process "${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe" -Wait -NoNewWindow -ArgumentList @(
  "--headless=new","--disable-gpu","--hide-scrollbars","--window-size=1920,1080",
  "--screenshot=$PWD\site\video\cartel.png","file:///$env:TEMP/cartel.html")
```

Ese `-Encoding UTF8` del `Get-Content` no es adorno: sin él, PowerShell 5.1 lee el SVG con la
página de códigos ANSI y el `·` del pie sale como `Â·` en la imagen. Y el texto va **apilado**,
no en línea con el logotipo, porque el ancho de «MultiDrive» depende de la fuente que tenga
delante quien rasterice, y en línea una fuente distinta descoloca el conjunto.

## Al cambiarlo

1. Sustituir el fichero con **el mismo nombre**, para no romper enlaces ni los dos README.
2. Anotar arriba la versión nueva.
3. Comprobar el *faststart* con la orden de más arriba.
4. Rehacer el cartel si deja de representarlo.
5. Publicar los dos: **no van en el despliegue del HTML**, igual que los logotipos. Ver
   [DESPLIEGUE.md](../../docs/DESPLIEGUE.md#el-spot).

## Por qué el README no lleva reproductor

Se probó, y no es cosa del sanitizador. `POST /markdown` --el mismo renderizador que pinta el
README-- **conserva** la etiqueta `<video>` con `src`, `width`, `controls` y `muted`. Parecía
que bastaba. No bastaba: lo que lo impide es la política de contenido de github.com.

```
media-src github.com user-images.githubusercontent.com secured-user-images.githubusercontent.com
          private-user-images.githubusercontent.com
          github-production-user-asset-6210df.s3.amazonaws.com
          gist.github.com github.githubassets.com
```

Solo dominios suyos. `multidrive.net` no está, así que el navegador bloquea el vídeo y la
etiqueta se queda sin alto: no sale ni el recuadro negro, no sale nada. Las **imágenes** sí
pasan porque GitHub las reenvía por su proxy (`camo.githubusercontent.com`, que sí está en
`img-src`); el vídeo no lo reenvía.

De ahí el cartel enlazado, que es una imagen y por tanto se ve siempre.

Para tener el reproductor dentro del README habría que alojar el fichero en GitHub, y eso solo
se consigue **subiéndolo desde el navegador**: arrastrarlo a una incidencia o a un comentario
devuelve una URL de `github.com/user-attachments/...`, que sí está permitida. No hay API
pública para eso, así que es un paso a mano y además deja el vídeo en dos sitios, con la
versión de GitHub sin forma de actualizarse cuando cambie el nuestro.
