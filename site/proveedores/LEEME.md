# Logotipos de los proveedores

Los que la portada enseña en «Con qué funciona».

## Por qué no están en `marca/`

Porque `marca/` es **nuestra** marca: el rombo, la paleta, el logotipo. Esto son marcas de
Google, Microsoft, Amazon, Nextcloud, Dropbox, Box y MEGA, y mezclarlas con las nuestras acaba
con alguien usando un logotipo ajeno donde debería ir el propio, o al revés.

Aparecen en la página solo para decir con qué habla MultiDrive. Ninguno de esos proyectos lo
patrocina ni está asociado con él, y eso se dice en la propia portada.

## Qué hay aquí

- Los `.png` de esta carpeta son **los que se publican**: 80 px de alto, que es el doble de los
  40 a los que se pintan, para que no se vean sucios en una pantalla de móvil.
- [`originales/`](originales) guarda las imágenes tal y como llegaron, de 512 a 1920 px. **No se
  publican**: el despliegue instala solo los `.png` de este nivel. Están para poder rehacer los
  pequeños si cambia el tamaño en la página, sin tener que volver a buscarlos.

## Rehacer los pequeños

Desde PowerShell, en la raíz del repositorio:

```powershell
Add-Type -AssemblyName System.Drawing
$alto = 80
Get-ChildItem site\proveedores\originales\*.png | ForEach-Object {
  $o = [System.Drawing.Image]::FromFile($_.FullName)
  $ancho = [int]([math]::Round($o.Width * $alto / $o.Height))
  $b = New-Object System.Drawing.Bitmap $ancho, $alto
  $g = [System.Drawing.Graphics]::FromImage($b)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.Clear([System.Drawing.Color]::Transparent)
  $g.DrawImage($o, 0, 0, $ancho, $alto)
  $b.Save((Join-Path $PWD "site\proveedores\$($_.Name)"), [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $b.Dispose(); $o.Dispose()
}
```

El nombre del fichero pequeño es el que pide `index.html`, que no siempre coincide con el del
original: `google-drive-768x768.png` se publica como `drive.png`, y `mega-nz.png` como
`mega.png`. Si se rehacen con el guion de arriba hay que renombrar esos dos.

## Al añadir uno nuevo

Tres sitios, y los tres hacen falta:

1. El original aquí, en `originales/`, y el pequeño en esta carpeta.
2. El `<img>` en el corro de `site/index.html`, con su `width` y `height` reales.
3. **Si el proveedor todavía no funciona**, la clase `pronto` en su `div`: eso lo pinta en gris
   y a media opacidad. Un logotipo a todo color dice «esto ya está» más alto que la palabra
   «en camino» que tenga debajo.
