# La marca de MultiDrive

Todo lo que hay aquí es la fuente. Si algo se ve mal en algún sitio, se arregla aquí y se
vuelve a generar; no se retoca la copia.

## El logotipo

Un rombo partido en cuatro cuñas que no llegan a tocarse. No es decoración: es lo que hace el
programa. `k` piezas de datos y `m` de paridad forman una sola figura, y la cuña de acento
—la superior derecha— es la paridad, lo que está de más para que se pueda perder algo.

| Fichero | Para qué |
|---|---|
| `logo.svg` | Sobre fondo oscuro. Es el de casi todos los sitios |
| `logo-claro.svg` | Sobre fondo claro: la figura pasa a tinta, el acento se queda |
| `logo-mono.svg` | A un solo color, heredado con `currentColor`. Sellos, serigrafía, impresión en negro |
| `icono.svg` | El rombo dentro de su cuadro. Favicon, icono de aplicación, avatar |
| `icono.ico` | Lo mismo para Windows: 16, 32, 48 y 256 px en un fichero |
| `png/icono-*.png` | Rasterizados, de 16 a 512 px, con transparencia |

**Área de respeto:** media altura del rombo por cada lado. Nada se mete ahí dentro.

**Tamaño mínimo:** 16 px. Por debajo, los huecos entre cuñas se cierran y deja de ser un
rombo partido para ser un rombo a secas.

Lo que no se hace: cambiarle los colores a las cuñas, rotarlo, estirarlo sin mantener la
proporción, ponerle sombra o degradado, ni meterlo sobre una foto. Si hace falta sobre una
foto, va el `icono.svg`, que ya trae su propio fondo.

## El nombre

**MultiDrive**, en una palabra, con la M y la D en mayúscula. Nunca «Multidrive», «multi
drive» ni «MULTIDRIVE» salvo dentro de una orden del terminal, donde todo va en minúscula:
`multidrive volume status`.

En el logotipo escrito, «Drive» va en color de acento y «Multi» en color de texto. Eso es
todo el truco tipográfico: la familia es la del sistema (`ui-sans-serif`), peso 700, con el
interletraje apretado a `-0.03em`. No hace falta ninguna fuente de pago ni descargar nada.

## Los colores

Están en `paleta.css` (variables listas para pegar) y en `paleta.json` (lo mismo, para
herramientas). Contraste medido sobre el fondo: texto 16,3:1, texto apagado 6,2:1, acento
5,9:1. Los tres pasan el nivel AA para texto normal; si alguien cambia un tono, vuelve a
medirlo antes de darlo por bueno.

El acento se usa **solo** en los títulos de sección, los enlaces y la cuña del logotipo. Un
acento en todas partes deja de ser un acento.

El ámbar `#d8a657` es exclusivamente del distintivo «pre-alfa». Cuando el proyecto deje de
serlo, ese color desaparece.

## Volver a generar los PNG y el .ico

Los PNG salen de `icono.svg` rasterizado a cada tamaño, y el `.ico` es la unión de los de 16,
32, 48 y 256. El procedimiento está en [`docs/DESPLIEGUE.md`](../docs/DESPLIEGUE.md).

## Licencia

El logotipo se distribuye con el proyecto, bajo AGPL-3.0 como el resto. Si publicas un
derivado, cámbiale la marca: que quien lo vea sepa quién lo mantiene y a quién reclamarle.
