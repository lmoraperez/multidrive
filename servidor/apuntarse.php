<?php

declare(strict_types=1);

/**
 * Recoge el correo de quien quiere entrar en las pruebas de Google Play.
 *
 * Vive **fuera** de la raíz del sitio, en `/var/www/multidrive-cgi`, y Caddy sólo le manda la
 * ruta `/apuntarse`. Eso no es manía: si el fichero estuviera dentro de `/var/www/multidrive`
 * y alguien pidiera `/apuntarse.php` por una ruta que no pasa por PHP, el servidor de ficheros
 * le devolvería **el código fuente**. Fuera de la raíz eso no puede pasar.
 *
 * Lo que se guarda es una línea por persona en un fichero que no sirve nadie. No hay base de
 * datos, no hay sesiones y no hay cookies: para una lista de testers que cabe en una pantalla,
 * cualquier otra cosa es montar un servicio que luego hay que mantener.
 */

// --------------------------------------------------------------------------------------
// Dónde se guarda
// --------------------------------------------------------------------------------------

/** La lista. Fuera de cualquier raíz web, y sólo la lee quien entra por SSH. */
const LISTA = '/var/lib/multidrive/betatesters.csv';

/** Marcas de tiempo por IP, para el límite de ritmo. Se puede borrar entero sin perder nada. */
const RITMO = '/var/lib/multidrive/ritmo';

/** Cuántos envíos se admiten desde la misma IP en una hora. */
const POR_HORA = 5;

/** Un correo de verdad no pasa de aquí (RFC 5321). Más largo es alguien probando cosas. */
const LARGO_MAXIMO = 254;

/** A quien se avisa de cada alta. Vacio apaga el aviso sin tocar nada mas. */
const AVISAR_A = 'lmoraperez@gmail.com';

/**
 * Desde que direccion sale el aviso.
 *
 * Tiene que ser del dominio que el servidor esta acreditado para firmar --`multidrive.net`,
 * con DKIM y con la IP en el SPF--. Puesta de otro dominio, Gmail la rechaza con un 5.7.26 y
 * el aviso no llega.
 */
const AVISAR_DESDE = 'avisos@multidrive.net';

// --------------------------------------------------------------------------------------
// Respuestas
// --------------------------------------------------------------------------------------

/**
 * Contesta con una página mínima y termina.
 *
 * **No se devuelve nada de lo que el visitante haya escrito.** Podría escaparse y enseñarlo,
 * pero no hace falta para nada y lo que no se imprime no se puede imprimir mal.
 */
function contestar(int $codigo, string $titulo, string $cuerpo, string $lang): never
{
    http_response_code($codigo);
    header('Content-Type: text/html; charset=utf-8');
    header('X-Robots-Tag: noindex');
    $volver = $lang === 'en' ? '/en/' : '/';
    $texto  = $lang === 'en' ? 'Back to MultiDrive' : 'Volver a MultiDrive';
    echo <<<HTML
    <!doctype html><html lang="{$lang}"><meta charset="utf-8">
    <meta name="viewport" content="width=device-width,initial-scale=1">
    <title>MultiDrive</title>
    <body style="font:16px/1.6 system-ui,sans-serif;background:#0d1017;color:#e8eef6;
    display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0">
    <div style="text-align:center;max-width:34rem;padding:0 1rem">
    <h1 style="font-weight:600">{$titulo}</h1>
    <p style="color:#8a94a6">{$cuerpo}</p>
    <p><a href="{$volver}" style="display:inline-block;margin-top:1rem;padding:.75rem 1.5rem;
    background:#4c8ff0;color:#0d1017;text-decoration:none;border-radius:.5rem;
    font-weight:700">{$texto}</a></p></div>
    HTML;
    exit;
}

// --------------------------------------------------------------------------------------
// Límite de ritmo
// --------------------------------------------------------------------------------------

/**
 * `true` si esta IP ya ha mandado demasiados en la última hora.
 *
 * La IP se guarda **resumida**, no en claro: para contar envíos basta con distinguir una de
 * otra, y no hace falta saber cuál es. El fichero se reescribe con lo que queda dentro de la
 * hora, así que se poda solo.
 *
 * Si algo falla al escribir, deja pasar. Un límite de ritmo roto que bloquea a todo el mundo
 * es peor que no tenerlo: lo que hay detrás es apuntarse a una lista, no una cuenta bancaria.
 */
function demasiados(string $ip): bool
{
    if (!is_dir(RITMO) && !@mkdir(RITMO, 0700, true) && !is_dir(RITMO)) {
        return false;
    }
    $f = RITMO . '/' . hash('sha256', $ip);
    $ahora = time();

    $previas = is_file($f) ? (array) json_decode((string) @file_get_contents($f), true) : [];
    $vivas = array_values(array_filter(
        $previas,
        static fn($t): bool => is_int($t) && $t > $ahora - 3600
    ));

    if (count($vivas) >= POR_HORA) {
        return true;
    }

    $vivas[] = $ahora;
    @file_put_contents($f, json_encode($vivas), LOCK_EX);
    return false;
}

// --------------------------------------------------------------------------------------
// El aviso
// --------------------------------------------------------------------------------------

/**
 * Avisa por correo de que alguien se ha apuntado.
 *
 * # Nunca estorba al que se apunta
 *
 * Va **despues** de guardar, y lo que falle aqui se traga en silencio. Si el correo no sale,
 * la persona ya esta en la lista y su pantalla dice que si; contarle que un aviso interno no
 * se mando seria darle un problema que no es suyo y que no puede resolver.
 *
 * # Por que no hay que escapar el correo en las cabeceras
 *
 * Porque llega despues de `FILTER_VALIDATE_EMAIL`, que no admite saltos de linea: sin salto
 * de linea no se puede inyectar una cabecera. Aun asi el asunto va sin el --sale en el
 * cuerpo-- porque un asunto es lo que mas viaja y lo mas facil de reenviar sin querer.
 */
function avisar(string $correo, string $lang): void
{
    if (AVISAR_A === '') {
        return;
    }

    $cuantos = is_file(LISTA) ? count(file(LISTA, FILE_SKIP_EMPTY_LINES) ?: []) : 0;

    $cuerpo = "Alguien se ha apuntado a las pruebas de Google Play.\n\n"
        . "  correo:  {$correo}\n"
        . "  idioma:  {$lang}\n"
        . "  fecha:   " . date('c') . "\n"
        . "  en lista: {$cuantos}\n\n"
        . "Para anadirlo: Play Console > Pruebas > Pruebas internas > Testers.\n"
        . "Tiene que ser esa misma cuenta de Google, que es a donde Google manda la invitacion.\n\n"
        . "La lista entera:\n"
        . "  ssh soporte@multidrive.net 'sudo cat " . LISTA . "'\n";

    $cabeceras = 'From: MultiDrive <' . AVISAR_DESDE . ">\r\n"
        . 'Reply-To: ' . $correo . "\r\n"
        . "Content-Type: text/plain; charset=utf-8\r\n";

    // El quinto argumento fija el remitente del sobre, que es lo que miran SPF y DMARC. Sin
    // el, Postfix lo pone del usuario del sistema --`www-data@`-- y la comprobacion no alinea
    // con la cabecera `From`, que es justo lo que hace que un correo acabe en spam.
    @mail(AVISAR_A, 'Nuevo apuntado a las pruebas de MultiDrive', $cuerpo, $cabeceras, '-f' . AVISAR_DESDE);
}

// --------------------------------------------------------------------------------------
// El flujo
// --------------------------------------------------------------------------------------

$lang = (($_POST['lang'] ?? $_GET['lang'] ?? 'es') === 'en') ? 'en' : 'es';
$en   = $lang === 'en';

if (($_SERVER['REQUEST_METHOD'] ?? 'GET') !== 'POST') {
    contestar(
        405,
        $en ? 'Nothing to see here' : 'Aquí no hay nada',
        $en
            ? 'This address only takes the sign-up form.'
            : 'Esta dirección sólo recibe el formulario para apuntarse.',
        $lang
    );
}

// La trampa. Un campo que la hoja de estilos esconde y que una persona no ve: si viene
// relleno, lo ha rellenado algo que lee el HTML y no la pantalla. Se contesta que todo bien
// --sin guardar nada-- porque decirle a un robot que se le ha pillado sólo sirve para que
// alguien afine el robot.
if (trim((string) ($_POST['telefono'] ?? '')) !== '') {
    contestar(
        200,
        $en ? 'Thanks' : 'Gracias',
        $en ? 'We have your address.' : 'Ya tenemos tu dirección.',
        $lang
    );
}

$correo = trim((string) ($_POST['correo'] ?? ''));

if ($correo === '' || strlen($correo) > LARGO_MAXIMO || !filter_var($correo, FILTER_VALIDATE_EMAIL)) {
    contestar(
        400,
        $en ? "That address doesn't look right" : 'Esa dirección no parece válida',
        $en
            ? 'Check it and try again. It has to be the Google account you use on the phone.'
            : 'Revísala y vuelve a intentarlo. Tiene que ser la cuenta de Google que usas en '
                . 'el teléfono.',
        $lang
    );
}

if (demasiados((string) ($_SERVER['REMOTE_ADDR'] ?? ''))) {
    contestar(
        429,
        $en ? 'Too many for now' : 'Demasiados por ahora',
        $en
            ? 'Several sign-ups have come from here in the last hour. Try again later, or '
                . 'write to gerencia@jlsoltech.com.'
            : 'De aquí han salido varios en la última hora. Inténtalo más tarde, o escribe a '
                . 'gerencia@jlsoltech.com.',
        $lang
    );
}

// Guardar. El correo se normaliza a minúsculas para que la comprobación de repetidos no
// dependa de cómo lo escribiera cada uno; Google tampoco distingue mayúsculas aquí.
$correo = mb_strtolower($correo);

$dir = dirname(LISTA);
if (!is_dir($dir) && !@mkdir($dir, 0700, true) && !is_dir($dir)) {
    contestar(
        500,
        $en ? 'Could not save it' : 'No se pudo guardar',
        $en
            ? 'Our fault, not yours. Write to gerencia@jlsoltech.com and you go on the list '
                . 'the same.'
            : 'Es cosa nuestra, no tuya. Escribe a gerencia@jlsoltech.com y entras en la '
                . 'lista igual.',
        $lang
    );
}

// Repetido: se dice, y no se añade otra vez. Quien vuelve a mandarlo suele ser quien no sabe
// si llego la primera vez, y contestarle «ya estabas» es la respuesta que buscaba.
if (is_file(LISTA)) {
    foreach (file(LISTA, FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES) ?: [] as $linea) {
        $campos = str_getcsv($linea, ',', '"', '\\');
        if (isset($campos[1]) && $campos[1] === $correo) {
            contestar(
                200,
                $en ? 'You were already on it' : 'Ya estabas en la lista',
                $en
                    ? 'That address is already down. You will get the invitation from Google '
                        . 'Play when the next round goes out.'
                    : 'Esa dirección ya estaba apuntada. Te llegará la invitación de Google '
                        . 'Play en la próxima tanda.',
                $lang
            );
        }
    }
}

$linea = sprintf(
    "%s,%s,%s\n",
    date('c'),
    // El correo ya pasó por FILTER_VALIDATE_EMAIL, así que no puede traer comas ni comillas.
    // Aun así va entrecomillado: la validación de hoy la cambia alguien mañana, y lo que no
    // depende de ella no se rompe cuando eso pase.
    '"' . str_replace('"', '""', $correo) . '"',
    $lang
);

$ok = @file_put_contents(LISTA, $linea, FILE_APPEND | LOCK_EX);
if ($ok === false) {
    contestar(
        500,
        $en ? 'Could not save it' : 'No se pudo guardar',
        $en
            ? 'Our fault, not yours. Write to gerencia@jlsoltech.com and you go on the list '
                . 'the same.'
            : 'Es cosa nuestra, no tuya. Escribe a gerencia@jlsoltech.com y entras en la '
                . 'lista igual.',
        $lang
    );
}
@chmod(LISTA, 0600);

avisar($correo, $lang);

contestar(
    200,
    $en ? 'You are on the list' : 'Estás en la lista',
    $en
        ? 'The invitation from Google Play arrives by email, and it is not instant: the list '
            . 'is added by hand. When it comes, open it on the phone and accept from there.'
        : 'La invitación de Google Play llega por correo, y no es inmediata: la lista se '
            . 'añade a mano. Cuando llegue, ábrela en el teléfono y acepta desde ahí.',
    $lang
);
