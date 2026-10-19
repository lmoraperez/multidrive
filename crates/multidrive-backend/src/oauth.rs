//! Conectar una cuenta desde el navegador.
//!
//! Lo que sustituye esto: antes, para usar OneDrive o Drive habia que registrar una
//! aplicacion en la consola del proveedor, correr un script que devolviera un token de
//! refresco, y pegar ese token en un formulario. Eso son tareas de desarrollador, y pedirselas
//! a quien solo quiere guardar sus ficheros no es una interfaz incompleta: es no tener
//! interfaz.
//!
//! # Como funciona
//!
//! Es el flujo de codigo de autorizacion con redireccion a `localhost`, que es el que usan
//! las aplicaciones de escritorio:
//!
//! 1. Se abre un servidor en el bucle local con un puerto que elige el sistema.
//! 2. Se manda el navegador a la pagina del proveedor, diciendole que al terminar redirija a
//!    ese puerto.
//! 3. El usuario entra en su cuenta y da permiso. **Sus credenciales no pasan por aqui**: van
//!    del navegador al proveedor, y este programa solo recibe un codigo de un solo uso.
//! 4. Ese codigo se canjea por un token de refresco, que es lo que se guarda.
//!
//! # Por que el puerto es el que sea
//!
//! Los dos proveedores admiten cualquier puerto en `127.0.0.1` sin declararlo por adelantado,
//! justamente para esto. Fijar uno seria fallar cuando ya estuviera ocupado.
//!
//! # PKCE
//!
//! Se manda un reto PKCE aunque la aplicacion lleve secreto y no sea obligatorio. El motivo es
//! concreto: el redirector es `localhost`, o sea que **cualquier otro programa de esta maquina
//! puede quedarse escuchando y recoger el codigo**. Sin PKCE le bastaria con eso; con PKCE el
//! codigo no sirve sin un valor que solo conoce este proceso y que nunca viaja por el
//! navegador.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::BackendError;
use crate::catalogo::Oauth;

/// Cuanto se espera a que el usuario termine en el navegador.
///
/// Cinco minutos da para entrar en la cuenta, buscar el segundo factor en el movil y leer la
/// pantalla de permisos. Pasado eso se suelta el puerto en vez de dejar un servidor abierto
/// para siempre: quien se fue a hacer otra cosa volvera y pulsara otra vez.
const ESPERA: Duration = Duration::from_secs(300);

/// Lo que lee el usuario en la pestana cuando el proveedor ya dio el permiso y lo que queda es
/// volver a la aplicacion.
///
/// Son constantes para que haya un test que vigile lo que dicen. No es puntillosidad: la
/// version anterior decia «Ya puedes volver / Terminando en MultiDrive…», y lo segundo era
/// falso --no terminaba nada hasta que el usuario pulsaba-- de una manera que deja a la gente
/// esperando a que la pantalla haga sola lo que no va a hacer.
const VUELTA_TITULO: &str = "Permiso concedido";
const VUELTA_DETALLE: &str = "Pulsa el botón para volver a MultiDrive y terminar.";

/// Una cuenta recien conectada.
///
/// El `Debug` se escribe a mano y redactado, por el mismo motivo que el de `OauthCreds`: un
/// `{:?}` descuidado en un registro o en la salida de un test fallido publicaria acceso
/// permanente a la cuenta.
pub struct Conectada {
    /// Lo unico que hay que guardar. Con el, el programa renueva el acceso solo.
    pub refresh_token: String,
}

/// Como se vuelve a la aplicacion desde el navegador. Solo hace falta en un movil.
///
/// Las dos cosas van juntas **a proposito**, y no por comodidad de tener un parametro menos.
/// Separadas estuvieron, y ese fue el fallo: se servia el boton de volver y despues se contaba
/// el tiempo a ojo --mil doscientos milisegundos-- confiando en que un salto automatico traeria
/// la aplicacion al frente dentro de ese plazo. Chrome bloquea ese salto. El plazo se agotaba
/// con la aplicacion todavia detras, el canje salia en segundo plano, y ahi es donde Android le
/// corta el DNS sin avisar. Quien pida lo uno tiene que dar lo otro.
pub struct Vuelta<'a> {
    /// El enlace que trae de vuelta a la aplicacion. Es lo que lleva el boton de la pagina.
    pub enlace: &'a str,

    /// Fabrica un futuro que termina cuando la aplicacion esta delante.
    ///
    /// Fabrica y no futuro ya hecho porque tiene que empezar a escuchar **despues** de servir
    /// la pagina. La aplicacion estaba delante hace nada, cuando el usuario pulso el boton de
    /// conectar; un futuro creado antes de tiempo se daria por cumplido con aquello y no
    /// habriamos esperado a nadie.
    ///
    /// Si nadie vuelve, tiene que terminar igual al cabo de un rato: un canje tardio todavia
    /// puede salir bien, y una espera sin fondo no.
    pub delante: &'a (dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Sync),
}

/// Abre el navegador, espera a que el usuario de permiso y devuelve el token de refresco.
///
/// `abrir` recibe la direccion a la que hay que mandar el navegador. Se pasa desde fuera para
/// poder probar el flujo entero sin abrir nada.
pub async fn conectar(
    oauth: &Oauth,
    client_id: &str,
    client_secret: &str,
    abrir: &(dyn Fn(&str) -> Result<(), String> + Sync),
    vuelta: Option<Vuelta<'_>>,
) -> Result<Conectada, BackendError> {
    let fallo = |detalle: String| BackendError::Oauth { detalle };

    // Puerto 0: lo elige el sistema. Se ata **antes** de mandar el navegador, para que no
    // haya una ventana en la que el proveedor redirija a un puerto que todavia no escucha.
    let servidor = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| fallo(format!("no se pudo abrir un puerto local: {e}")))?;
    let puerto = servidor
        .local_addr()
        .map_err(|e| fallo(e.to_string()))?
        .port();

    // Con `localhost` delante hay que escuchar tambien en `::1`: en Windows ese nombre resuelve
    // antes a IPv6, y el navegador se encontraria el puerto cerrado. Es el **mismo** puerto,
    // atado aparte porque son dos familias distintas; si en IPv6 estuviera ocupado se sigue
    // con IPv4 sola, que es exactamente lo que habia antes.
    //
    // Con `127.0.0.1` no se plantea, y por eso no se ata un socket de mas sin necesidad.
    let servidor6 = if oauth.anfitrion == "localhost" {
        TcpListener::bind(("::1", puerto)).await.ok()
    } else {
        None
    };

    let redirigir = format!("http://{}:{puerto}", oauth.anfitrion);

    let verificador = aleatorio();
    let reto = reto_de(&verificador);
    let estado = aleatorio();

    let mut url = format!(
        "{}?client_id={}&response_type=code&redirect_uri={}&scope={}&state={}\
         &code_challenge={reto}&code_challenge_method=S256",
        oauth.autorizar,
        urlencode(client_id),
        urlencode(&redirigir),
        urlencode(oauth.ambito),
        urlencode(&estado),
    );
    for (clave, valor) in oauth.extra {
        url.push_str(&format!("&{clave}={}", urlencode(valor)));
    }

    abrir(&url).map_err(|e| {
        fallo(format!(
            "no se pudo abrir el navegador: {e}\nAbre esta direccion a mano:\n{url}"
        ))
    })?;

    let (codigo, mut socket) = tokio::time::timeout(
        ESPERA,
        esperar_codigo(&servidor, servidor6.as_ref(), &estado),
    )
    .await
    // Se dice **donde se queda la gente**, no solo que se acabo el tiempo. La primera vez que
    // esto fallo de verdad, la causa fue que el boton de continuar de la pantalla de permisos
    // de Google queda debajo del pliegue en un movil: quien no lo sabe lee lo que hay, no ve
    // ningun boton y se queda esperando. «Se agoto la espera» es cierto y no sirve de nada.
    .map_err(|_| {
        fallo(
            "se agoto la espera: no llego el permiso.\n\n\
             Si la pantalla del proveedor llego a pedirtelo, hay que bajar hasta el final y \
             pulsar el boton de continuar: en una pantalla pequena queda fuera de la vista.\n\n\
             Puedes intentarlo otra vez."
                .into(),
        )
    })??;

    // El canje va **antes** de contestarle al navegador, y no es un detalle de orden.
    //
    // Antes se servia la pagina de «Cuenta conectada» nada mas recibir el codigo, y solo
    // despues se canjeaba. Si el canje fallaba, el usuario ya habia leido en su pestana que
    // estaba conectado mientras la ventana se quedaba igual: la pestana afirmaba algo que
    // todavia no se sabia.
    //
    // Ahora la pestana dice lo que ha pasado de verdad, y cuando falla **ensena el motivo
    // ahi mismo**, que es donde el usuario esta mirando.
    // # Con vuelta (el movil) el orden es **el contrario**, y por un motivo de peso
    //
    // Todo lo de arriba vale cuando el usuario se queda en la pestana. En un movil no: ahi
    // quien esta delante es el navegador, y nuestra aplicacion esta **en segundo plano**.
    //
    // Y a una aplicacion en segundo plano Android le puede cortar la red --ahorro de bateria,
    // ahorro de datos, una aplicacion marcada como restringida, un VPN que no la enruta-- sin
    // avisar. Cuando lo hace, el sintoma no es «red no disponible»: es que el DNS deja de
    // resolver, con un «no address associated with hostname» que parece un problema del
    // proveedor y no lo es. Un tester se comio exactamente eso.
    //
    // Asi que primero se responde --lo que le da al usuario la manera de volver-- y **despues**
    // se canjea, ya con la aplicacion delante.
    //
    // # No se cuenta el tiempo: se espera a que vuelva
    //
    // Aqui hubo una pausa de mil doscientos milisegundos, puesta para darle tiempo al salto
    // automatico. Medido en un telefono: el salto **no llega**, porque Chrome no deja que una
    // pagina mande a otra aplicacion sin que medie un gesto, y entonces la pausa terminaba con
    // la aplicacion todavia detras y el canje salia de fondo igual. El arreglo no arreglaba
    // nada, y de la peor manera: pareciendolo.
    //
    // Ahora se espera a la señal de que la aplicacion esta delante de verdad, venga del salto
    // o del dedo del usuario en el boton. Quien la da es quien puede saberlo, que es la capa
    // de Android.
    //
    // La pagina no puede decir «cuenta conectada», porque todavia no se sabe. Dice lo unico
    // que es cierto en ese momento --el proveedor dio el permiso-- y lo que hay que hacer. El
    // resultado de verdad lo cuenta la aplicacion, que es donde el usuario acaba de aterrizar.
    if let Some(vuelta) = &vuelta {
        // La espera se arma **antes** de servir la pagina, y el orden importa: entre servirla
        // y ponerse a esperar cabe la vuelta del usuario, y una espera armada tarde se pierde
        // justo el aviso por el que esperaba. Lo que se arma aqui es el punto de partida; lo
        // que se mira debajo es si ha pasado algo desde este instante.
        let vuelto = (vuelta.delante)();
        responder_pagina(&mut socket, VUELTA_TITULO, VUELTA_DETALLE, Some(vuelta.enlace)).await;
        vuelto.await;
        return canjear(
            oauth,
            client_id,
            client_secret,
            &codigo,
            &redirigir,
            &verificador,
        )
        .await;
    }

    match canjear(
        oauth,
        client_id,
        client_secret,
        &codigo,
        &redirigir,
        &verificador,
    )
    .await
    {
        Ok(c) => {
            responder_pagina(
                &mut socket,
                "Cuenta conectada",
                "Ya puedes cerrar esta pestaña y volver a MultiDrive.",
                None,
            )
            .await;
            Ok(c)
        }
        Err(e) => {
            // Al fallar **no** se vuelve solo: el motivo está en esta página y hay que poder
            // leerlo. Saltar a la aplicación lo dejaría atrás sin que nadie lo viera.
            responder_pagina(&mut socket, "No se pudo conectar", &e.to_string(), None).await;
            Err(e)
        }
    }
}

/// Acepta por cualquiera de los dos escuchas, que son el mismo puerto en dos familias.
async fn aceptar(
    v4: &TcpListener,
    v6: Option<&TcpListener>,
) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
    match v6 {
        None => v4.accept().await,
        Some(v6) => tokio::select! {
            r = v4.accept() => r,
            r = v6.accept() => r,
        },
    }
}

/// Atiende hasta que llegue la redireccion buena.
///
/// Se sigue escuchando tras una peticion que no trae codigo porque el navegador pide cosas
/// por su cuenta —`/favicon.ico` es la clasica— y cortar en la primera conexion dejaria el
/// flujo colgado por una peticion que no es la del usuario.
async fn esperar_codigo(
    servidor: &TcpListener,
    servidor6: Option<&TcpListener>,
    estado: &str,
) -> Result<(String, tokio::net::TcpStream), BackendError> {
    loop {
        let (mut socket, _) =
            aceptar(servidor, servidor6)
                .await
                .map_err(|e| BackendError::Oauth {
                    detalle: e.to_string(),
                })?;

        let mut buf = vec![0u8; 8192];
        let leidos = socket.read(&mut buf).await.unwrap_or(0);
        let peticion = String::from_utf8_lossy(&buf[..leidos]).to_string();
        let ruta = peticion
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("")
            .to_string();

        if ruta.starts_with("/favicon.ico") {
            responder(&mut socket, 404, "no").await;
            continue;
        }

        let params = consulta(&ruta);
        let buscar = |c: &str| params.iter().find(|(k, _)| k == c).map(|(_, v)| v.clone());

        if let Some(error) = buscar("error") {
            let detalle = buscar("error_description").unwrap_or_else(|| error.clone());
            responder_pagina(&mut socket, "No se pudo conectar", &detalle, None).await;
            return Err(BackendError::Oauth {
                detalle: format!("el proveedor rechazo la peticion: {error}: {detalle}"),
            });
        }

        let Some(codigo) = buscar("code") else {
            // Cualquier otra peticion suelta al puerto. Ni se contesta con la pagina buena ni
            // se corta el flujo.
            responder(&mut socket, 400, "esto no es la redireccion esperada").await;
            continue;
        };

        // El `state` es lo que ata la respuesta a la peticion que hizo este proceso. Sin
        // comprobarlo, otro programa de la maquina podria empujar un codigo suyo a este
        // puerto y hacer que se conectara **su** cuenta en vez de la del usuario.
        if buscar("state").as_deref() != Some(estado) {
            responder_pagina(
                &mut socket,
                "No se pudo conectar",
                "la respuesta no corresponde a esta peticion",
                None,
            )
            .await;
            return Err(BackendError::Oauth {
                detalle: "la respuesta no corresponde a esta peticion (state distinto)".into(),
            });
        }

        // La pagina no se sirve aqui: lo que ha pasado todavia no se sabe. La contesta quien
        // canjea el codigo, que es quien lo averigua.
        return Ok((codigo, socket));
    }
}

/// Por que fallo un canje, que es lo que decide si repetirlo sirve de algo.
///
/// La frontera no es «error de red» contra «error del proveedor»: es **si la peticion llego a
/// salir**. Un codigo de autorizacion es de un solo uso, y repetir uno que el proveedor ya vio
/// no da otra respuesta, da la misma con peor cara. Pero si la peticion nunca salio, el codigo
/// sigue sin gastar y repetirla es seguro.
enum FalloDeCanje {
    /// No se pudo ni mandar: DNS, conexion rechazada, tiempo agotado. El codigo sigue intacto.
    Transporte(BackendError),
    /// El proveedor contesto, y contesto que no. Repetirlo no cambia nada.
    Respondio(BackendError),
}

impl From<FalloDeCanje> for BackendError {
    fn from(f: FalloDeCanje) -> Self {
        match f {
            FalloDeCanje::Transporte(e) | FalloDeCanje::Respondio(e) => e,
        }
    }
}

/// Lo que se espera entre un intento y el siguiente.
///
/// # Por que hay reintentos
///
/// Medido en un telefono, con el ahorro de datos encendido: el usuario pulsa «Volver a
/// MultiDrive», la pantalla resume, y el canje sale en el mismo instante en que el sistema
/// esta abriendo el cortafuegos de la aplicacion. El registro de `netpolicy` y el resume caen
/// dentro de la misma decima de segundo, y la consulta de DNS se cuela por delante: «No
/// address associated with hostname», que es exactamente el error que reporto un tester.
///
/// Esperar a tener la aplicacion delante era necesario y no es suficiente: «delante» y «con
/// red» no ocurren a la vez. No hay nada que observar para saber cuando la red esta lista --no
/// hay aviso de que el cortafuegos se abrio-- asi que la unica manera de saberlo es intentarlo.
///
/// Tres esperas y no una: la primera cubre la carrera, que es cosa de decimas; las otras dos
/// cubren el caso de que el sistema tarde mas de lo visto. Si a los cuatro segundos largos
/// sigue sin haber red, es que no la hay, y entonces el mensaje de error --que nombra el VPN,
/// el ahorro de bateria y el de datos-- vale mas que seguir esperando.
const ESPERAS_ENTRE_CANJES: [Duration; 3] = [
    Duration::from_millis(400),
    Duration::from_millis(1200),
    Duration::from_millis(3000),
];

/// Canjea el codigo, repitiendo si la peticion no llego a salir.
async fn canjear(
    oauth: &Oauth,
    client_id: &str,
    client_secret: &str,
    codigo: &str,
    redirigir: &str,
    verificador: &str,
) -> Result<Conectada, BackendError> {
    let mut ultimo = match canjear_una_vez(oauth, client_id, client_secret, codigo, redirigir, verificador).await {
        Ok(c) => return Ok(c),
        Err(FalloDeCanje::Respondio(e)) => return Err(e),
        Err(FalloDeCanje::Transporte(e)) => e,
    };

    for espera in ESPERAS_ENTRE_CANJES {
        tokio::time::sleep(espera).await;
        match canjear_una_vez(oauth, client_id, client_secret, codigo, redirigir, verificador).await
        {
            Ok(c) => return Ok(c),
            Err(FalloDeCanje::Respondio(e)) => return Err(e),
            // Se guarda el ultimo y no el primero: si la causa cambia por el camino, lo que le
            // sirve a quien lo lee es como acabo la cosa, no como empezo.
            Err(FalloDeCanje::Transporte(e)) => ultimo = e,
        }
    }

    Err(ultimo)
}

async fn canjear_una_vez(
    oauth: &Oauth,
    client_id: &str,
    client_secret: &str,
    codigo: &str,
    redirigir: &str,
    verificador: &str,
) -> Result<Conectada, FalloDeCanje> {
    // El del proceso, que en Android se construye con otro almacen de confianza. Fabricar uno
    // aqui dejaria el canje de codigo fuera del arreglo, y es justo la peticion sin la que no
    // se puede conectar ninguna cuenta.
    // Construir el cliente puede fallar por el almacen de confianza, y eso no se arregla
    // repitiendolo. Pero tampoco llega a mandar nada, asi que el codigo queda sin gastar: va
    // por transporte, que es la categoria que mira justo eso.
    let cliente =
        crate::http::construir(Duration::from_secs(30)).map_err(FalloDeCanje::Transporte)?;

    let respuesta = cliente
        .post(oauth.token)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", codigo),
            ("redirect_uri", redirigir),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code_verifier", verificador),
        ])
        .send()
        .await
        .map_err(|e| {
            FalloDeCanje::Transporte(BackendError::Oauth {
                detalle: crate::http::con_causas(&e),
            })
        })?;

    let estado = respuesta.status();
    let cuerpo = respuesta.text().await.unwrap_or_default();
    if !estado.is_success() {
        // Recortado y sin caracteres de control: este mensaje acaba en registros y en
        // capturas de pantalla, y el cuerpo de un error de OAuth puede traer trozos de lo
        // enviado.
        let pista: String = cuerpo
            .chars()
            .filter(|c| !c.is_control())
            .take(300)
            .collect();
        return Err(FalloDeCanje::Respondio(BackendError::Oauth {
            detalle: format!("el proveedor respondio {estado}: {pista}"),
        }));
    }

    let json: serde_json::Value =
        serde_json::from_str(&cuerpo).map_err(|e| {
            FalloDeCanje::Respondio(BackendError::Oauth {
                detalle: e.to_string(),
            })
        })?;
    let refresh_token = json["refresh_token"]
        .as_str()
        .ok_or_else(|| {
            FalloDeCanje::Respondio(BackendError::Oauth {
                // Pasa de verdad y tiene arreglo concreto, asi que se dice cual: Google no
                // reenvia el token de refresco a quien ya le habia dado permiso antes, salvo
                // que se le pida el consentimiento otra vez.
                detalle: "el proveedor no devolvio ningun token de refresco. Si ya habias \
                          conectado esta cuenta, retirale el permiso a la aplicacion en los \
                          ajustes de tu cuenta y vuelve a conectarla"
                    .into(),
            })
        })?;

    Ok(Conectada {
        refresh_token: refresh_token.to_string(),
    })
}

// ---------------------------------------------------------------------------------------
// Navegador
// ---------------------------------------------------------------------------------------

/// Abre la direccion en el navegador que tenga puesto el usuario.
///
/// **En Windows no se pasa por `cmd`**, y eso no es una preferencia de estilo: es un fallo
/// que costo una pantalla de "Acceso bloqueado" de Google. La version anterior hacia
/// `cmd /C start "" <url>` confiando en que Rust entrecomillaria el argumento. Rust solo
/// entrecomilla lo que lleva espacios, y una direccion no lleva; asi que `cmd` veia el `&`
/// --que para el separa ordenes-- y **cortaba la direccion por el primer parametro**. Google
/// recibia `?client_id=...` a secas y contestaba `Required parameter is missing:
/// response_type`.
///
/// `ShellExecuteW` es la llamada del sistema que hay debajo de "abrir con el programa
/// asociado". No hay linea de ordenes que reinterpretar, asi que esa clase de fallo
/// desaparece en vez de quedar arreglada con mejores comillas.
#[cfg(windows)]
pub fn abrir_en_navegador(url: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let ancho = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(Some(0))
            .collect()
    };
    let operacion = ancho("open");
    let fichero = ancho(url);

    // SEGURIDAD: los dos vectores acaban en cero y viven durante toda la llamada. El resto de
    // punteros son opcionales y van nulos, que es lo que la documentacion pide para abrir
    // algo sin parametros ni directorio de trabajo.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operacion.as_ptr(),
            fichero.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    // La documentacion lo dice asi de raro: devuelve un valor **mayor que 32** si salio bien,
    // y un codigo de error si no. No es un puntero aunque lo parezca por el tipo.
    if r as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW devolvio {}", r as isize))
    }
}

#[cfg(not(windows))]
pub fn abrir_en_navegador(url: &str) -> Result<(), String> {
    // Aqui el argumento va en el vector de argumentos del proceso, sin interprete de por
    // medio, asi que la direccion llega entera.
    let programa = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(programa)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------
// Piezas
// ---------------------------------------------------------------------------------------

/// Valor aleatorio, en la forma que admiten `state` y el verificador PKCE.
///
/// 32 bytes del generador del sistema, escritos en base64 de URL. Salen 43 caracteres, que
/// es justo el minimo que pide el RFC 7636, y todos son de los que ese RFC permite sin
/// escapar: no hay que codificarlo al meterlo en la direccion.
fn aleatorio() -> String {
    base64_url(&rand::random::<[u8; 32]>())
}

/// El reto PKCE: SHA-256 del verificador, en base64 de URL y sin relleno.
fn reto_de(verificador: &str) -> String {
    let resumen = Sha256::digest(verificador.as_bytes());
    base64_url(&resumen)
}

fn base64_url(bytes: &[u8]) -> String {
    const TABLA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for trozo in bytes.chunks(3) {
        let b = [
            trozo[0],
            trozo.get(1).copied().unwrap_or(0),
            trozo.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        // Sin relleno: el RFC 7636 lo exige asi, y un `=` de mas hace que el proveedor
        // calcule otro resumen y rechace el canje.
        for i in 0..trozo.len() + 1 {
            out.push(TABLA[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

/// Escapa lo que va en la parte de consulta de una URL.
///
/// A mano y no con una caja: son cuatro reglas y aqui solo pasan direcciones y valores que
/// produce este mismo programa.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Deshace el escapado de un valor de la consulta.
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(b) => out.push(b),
                    // Un `%` suelto se deja tal cual en vez de tirar la respuesta entera: el
                    // valor que importa es el codigo, y perderlo por un caracter raro en otro
                    // parametro obligaria a repetir todo el flujo.
                    Err(_) => out.push(b'%'),
                }
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Los pares `clave=valor` de la parte de consulta de una ruta.
fn consulta(ruta: &str) -> Vec<(String, String)> {
    let Some((_, cola)) = ruta.split_once('?') else {
        return Vec::new();
    };
    cola.split('&')
        .filter_map(|par| {
            let (k, v) = par.split_once('=')?;
            Some((urldecode(k), urldecode(v)))
        })
        .collect()
}

async fn responder(socket: &mut tokio::net::TcpStream, codigo: u16, texto: &str) {
    let cuerpo = texto.as_bytes();
    let cabecera = format!(
        "HTTP/1.1 {codigo} .\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        cuerpo.len()
    );
    socket.write_all(cabecera.as_bytes()).await.ok();
    socket.write_all(cuerpo).await.ok();
    socket.shutdown().await.ok();
}

/// La pagina que ve el usuario al volver del proveedor.
///
/// Se sirve desde aqui y no se redirige a ningun sitio: **esta pestana ya lleva el codigo en
/// la direccion**, y mandarla a una pagina externa se lo entregaria a ese servidor en la
/// cabecera de procedencia.
/// La pagina que ve el navegador al terminar.
///
/// # `volver_a`: devolver al usuario a la aplicacion
///
/// En el escritorio no hace falta --la ventana sigue ahi detras-- y se pasa `None`. En un
/// movil si: sin esto hay que cerrar la pestana, buscar la aplicacion y abrirla, para algo
/// que el navegador podria hacer solo.
///
/// Se intenta de **dos** maneras, y hay que saber cual de las dos es la que trabaja:
///
/// - Un boton bien visible con el enlace. **Esta es la que funciona**, y por eso el texto de
///   la pagina manda pulsarlo en vez de prometer que no hara falta.
/// - Un `iframe` oculto apuntando al mismo sitio. Es un tiro a puerta, no el plan.
///
/// Chrome bloquea el salto a otra aplicacion si no hubo un gesto del usuario, y desde un
/// `iframe` lo bloquea siempre. Comprobado en un telefono: no salta. Se deja porque hay
/// navegadores que si lo permiten y ahi ahorra un toque, pero la pagina no cuenta con ello y
/// el texto no lo menciona: prometer un salto que no ocurre deja a la gente esperando delante
/// de una pantalla que dice que ya casi esta.
///
/// Que el intento vaya en un `iframe` y no en un `location.replace` es lo que hace que fallar
/// no cueste nada: un iframe bloqueado **no toca la pagina** y el boton sigue ahi. Con
/// `location.replace` el fallo se lleva por delante la pagina y el boton con ella, y entonces
/// el usuario esta peor que antes de intentarlo.
///
/// El guion no lleva la URL escrita: la lee del `href` del boton. Asi solo hay que escaparla
/// para un atributo HTML, y no ademas para una cadena de JavaScript.
async fn responder_pagina(
    socket: &mut tokio::net::TcpStream,
    titulo: &str,
    detalle: &str,
    volver_a: Option<&str>,
) {
    let cuerpo = pagina(titulo, detalle, volver_a);
    let bytes = cuerpo.as_bytes();
    let cabecera = format!(
        "HTTP/1.1 200 .\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    socket.write_all(cabecera.as_bytes()).await.ok();
    socket.write_all(bytes).await.ok();
    socket.shutdown().await.ok();
}

/// El HTML de la pagina, aparte de mandarlo, para poder comprobarlo sin levantar un socket.
fn pagina(titulo: &str, detalle: &str, volver_a: Option<&str>) -> String {
    let vuelta = match volver_a {
        None => String::new(),
        Some(url) => format!(
            "<p><a id=\"v\" href=\"{}\" style=\"display:inline-block;margin-top:1rem;\
             padding:.75rem 1.5rem;background:#4c8ff0;color:#fff;text-decoration:none;\
             border-radius:.5rem;font-weight:600\">Volver a MultiDrive</a></p>\
             <iframe id=\"f\" style=\"display:none\" title=\"\"></iframe>\
             <script>document.getElementById('f').src=document.getElementById('v').href\
             </script>",
            escapar_atributo(url)
        ),
    };

    format!(
        "<!doctype html><html lang=\"es\"><meta charset=\"utf-8\">\
         <title>MultiDrive</title>\
         <body style=\"font:16px/1.6 system-ui,sans-serif;background:#0e1116;color:#e6edf3;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <div style=\"text-align:center;max-width:32rem;padding:0 1rem\">\
         <h1 style=\"font-weight:600\">{}</h1><p style=\"color:#8b949e\">{}</p>{vuelta}</div>",
        escapar(titulo),
        escapar(detalle)
    )
}

/// Parte del detalle viene del proveedor, asi que no se mete en HTML tal cual.
fn escapar(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Lo mismo, para dentro de un atributo entre comillas dobles.
///
/// Una comilla de mas ahi no estropea el texto: cierra el atributo y lo que venga detras pasa
/// a ser marcado. La URL de vuelta la ponemos nosotros, pero el escape no depende de eso.
fn escapar_atributo(s: &str) -> String {
    escapar(s).replace('"', "&quot;")
}

impl std::fmt::Debug for Conectada {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conectada")
            .field("refresh_token", &"<redactado>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El ejemplo del RFC 7636, apendice B. Si esto no sale bordado, el proveedor calcula
    /// otro resumen y rechaza el canje con un error que no dice por que.
    #[test]
    fn el_reto_pkce_es_el_del_rfc() {
        let verificador = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            reto_de(verificador),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn el_base64_de_url_no_lleva_relleno_ni_caracteres_de_los_otros() {
        // Un `=` de mas, o un `+` en vez de un `-`, y el proveedor rechaza el canje.
        for n in 1..40usize {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            let s = base64_url(&bytes);
            assert!(!s.contains('='), "{s}");
            assert!(!s.contains('+'), "{s}");
            assert!(!s.contains('/'), "{s}");
        }
    }

    #[test]
    fn los_valores_aleatorios_no_se_repiten_ni_hay_que_escaparlos() {
        let a = aleatorio();
        let b = aleatorio();
        assert_ne!(a, b);
        assert_eq!(
            urlencode(&a),
            a,
            "el alfabeto elegido no necesita escaparse"
        );
        assert!(a.len() >= 43, "el RFC 7636 pide al menos 43 caracteres");
    }

    #[test]
    fn se_leen_los_parametros_de_la_redireccion() {
        let p = consulta("/?code=4%2F0Ab_c&state=xyz&scope=https%3A%2F%2Fa.b%2Fc");
        let buscar = |c: &str| p.iter().find(|(k, _)| k == c).map(|(_, v)| v.as_str());
        // El codigo de Google lleva `/`, que viaja escapado. Devolverlo sin deshacer el
        // escapado haria que el canje fallara con "codigo invalido".
        assert_eq!(buscar("code"), Some("4/0Ab_c"));
        assert_eq!(buscar("state"), Some("xyz"));
        assert_eq!(buscar("scope"), Some("https://a.b/c"));
    }

    #[test]
    fn una_ruta_sin_consulta_no_da_parametros_ni_revienta() {
        assert!(consulta("/").is_empty());
        assert!(consulta("").is_empty());
    }

    #[test]
    fn lo_que_venga_del_proveedor_no_se_cuela_como_html() {
        // El detalle del error lo escribe el proveedor. Ponerlo tal cual en la pagina seria
        // dejarle escribir HTML en una pagina servida desde esta maquina.
        assert_eq!(
            escapar("<script>alert(1)</script>"),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }

    /// Un proveedor de mentira: atiende el canje y devuelve lo que se le diga.
    ///
    /// Devuelve tambien lo que recibio, para poder comprobar que se le mando lo que el
    /// protocolo exige y no solo que el flujo termina.
    async fn proveedor_falso(
        respuesta: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let servidor = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let puerto = servidor.local_addr().unwrap().port();
        let (avisar, recibido) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut socket, _) = servidor.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            avisar
                .send(String::from_utf8_lossy(&buf[..n]).to_string())
                .ok();
            let cabecera = format!(
                "HTTP/1.1 200 .\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                respuesta.len()
            );
            socket.write_all(cabecera.as_bytes()).await.ok();
            socket.write_all(respuesta.as_bytes()).await.ok();
            socket.shutdown().await.ok();
        });

        (format!("http://127.0.0.1:{puerto}/token"), recibido)
    }

    /// Hace de navegador: saca de la direccion el puerto y el `state`, y llama de vuelta.
    fn navegador_que_acepta(url: &str) -> Result<(), String> {
        let params = consulta(url);
        let buscar = |c: &str| {
            params
                .iter()
                .find(|(k, _)| k == c)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let vuelta = buscar("redirect_uri");
        let estado = buscar("state");
        tokio::spawn(async move {
            let destino = vuelta.trim_start_matches("http://");
            let mut socket = tokio::net::TcpStream::connect(destino).await.unwrap();
            let peticion =
                format!("GET /?code=un-codigo&state={estado} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            socket.write_all(peticion.as_bytes()).await.unwrap();
            let mut basura = Vec::new();
            socket.read_to_end(&mut basura).await.ok();
        });
        Ok(())
    }

    fn oauth_falso(token: &str) -> Oauth {
        con_anfitrion(token, "127.0.0.1")
    }

    fn con_anfitrion(token: &str, anfitrion: &'static str) -> Oauth {
        Oauth {
            autorizar: "http://127.0.0.1:1/autorizar",
            token: Box::leak(token.to_string().into_boxed_str()),
            ambito: "un.ambito",
            anfitrion,
            cuota: "http://127.0.0.1:1/cuota",
            identidad: "http://127.0.0.1:1/identidad",
            campo_identidad: &["id"],
            extra: &[("access_type", "offline")],
        }
    }

    /// El flujo entero: abrir puerto, mandar el navegador, recoger el codigo y canjearlo.
    #[tokio::test]
    async fn se_conecta_una_cuenta_de_punta_a_punta() {
        let (url, recibido) =
            proveedor_falso(r#"{"access_token":"a","refresh_token":"el-bueno","expires_in":3599}"#)
                .await;
        let oauth = oauth_falso(&url);

        let c = conectar(&oauth, "id-de-la-app", "secreto", &navegador_que_acepta, None)
            .await
            .unwrap();
        assert_eq!(c.refresh_token, "el-bueno");

        // Y se comprueba que el canje llevaba lo que el protocolo exige. Sin el
        // `code_verifier`, el proveedor de verdad rechaza el canje con un error que no dice
        // por que; sin el `redirect_uri`, tambien.
        let peticion = recibido.await.unwrap();
        for exigido in [
            "grant_type=authorization_code",
            "code=un-codigo",
            "code_verifier=",
            "redirect_uri=",
            "client_id=id-de-la-app",
        ] {
            assert!(
                peticion.contains(exigido),
                "falta {exigido} en:\n{peticion}"
            );
        }
    }

    /// Cada proveedor escribe la redireccion con el nombre que tiene registrado.
    ///
    /// No es cosmetico: la redireccion se compara **como texto** contra la registrada, y
    /// mandar `127.0.0.1` donde hay puesto `localhost` se rechaza con un error de redireccion
    /// que no dice que el problema sea el nombre.
    #[tokio::test]
    async fn la_redireccion_lleva_el_anfitrion_del_proveedor() {
        for anfitrion in ["127.0.0.1", "localhost"] {
            let (url, recibido) = proveedor_falso(
                r#"{"access_token":"a","refresh_token":"el-bueno","expires_in":3599}"#,
            )
            .await;
            let oauth = con_anfitrion(&url, anfitrion);

            let c = conectar(&oauth, "id-de-la-app", "secreto", &navegador_que_acepta, None)
                .await
                .unwrap_or_else(|e| panic!("con {anfitrion}: {e}"));
            assert_eq!(c.refresh_token, "el-bueno");

            // Y va tal cual en el canje, que es donde el proveedor lo vuelve a comparar.
            let peticion = recibido.await.unwrap();
            let esperado = urlencode(&format!("http://{anfitrion}:"));
            assert!(
                peticion.contains(&format!("redirect_uri={esperado}")),
                "con {anfitrion} el canje mando otra cosa:
{peticion}"
            );
        }
    }

    /// Hace de navegador y **se queda con lo que le contesten**, para poder mirarlo.
    fn navegador_que_guarda_la_respuesta(
        url: &str,
    ) -> (Result<(), String>, tokio::sync::oneshot::Receiver<String>) {
        let params = consulta(url);
        let buscar = |c: &str| {
            params
                .iter()
                .find(|(k, _)| k == c)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        let vuelta = buscar("redirect_uri");
        let estado = buscar("state");
        let (avisar, pagina) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let destino = vuelta.trim_start_matches("http://").to_string();
            let mut socket = tokio::net::TcpStream::connect(&destino).await.unwrap();
            let peticion =
                format!("GET /?code=un-codigo&state={estado} HTTP/1.1\r\nHost: x\r\n\r\n");
            socket.write_all(peticion.as_bytes()).await.unwrap();
            let mut respuesta = Vec::new();
            socket.read_to_end(&mut respuesta).await.ok();
            avisar
                .send(String::from_utf8_lossy(&respuesta).to_string())
                .ok();
        });
        (Ok(()), pagina)
    }

    /// **La pestana no puede decir que la cuenta esta conectada antes de que lo este.**
    ///
    /// Es el fallo que aparecio probando contra Google: la pagina de exito se servia nada mas
    /// recibir el codigo, y el canje venia despues. Cuando el canje fallaba, el usuario leia
    /// «Cuenta conectada» en su pestana mientras la ventana se quedaba igual, y lo que veia
    /// era una contradiccion sin explicacion.
    #[tokio::test]
    async fn si_el_canje_falla_la_pestana_no_dice_que_esta_conectada() {
        // El proveedor contesta sin token de refresco: el canje no sale.
        let (url, _recibido) = proveedor_falso(r#"{"access_token":"solo-este"}"#).await;
        let oauth = oauth_falso(&url);

        let pagina = std::sync::Mutex::new(None);
        let abrir = |u: &str| -> Result<(), String> {
            let (r, canal) = navegador_que_guarda_la_respuesta(u);
            *pagina.lock().unwrap() = Some(canal);
            r
        };

        let e = conectar(&oauth, "id", "secreto", &abrir, None).await.unwrap_err();
        // El cerrojo se suelta antes de esperar: sostenerlo a traves de un `await` es lo
        // que convierte un test en un bloqueo intermitente.
        let canal = pagina.lock().unwrap().take().unwrap();
        let vista = canal.await.unwrap();

        assert!(
            !vista.contains("Cuenta conectada"),
            "la pestana mintio:\n{vista}"
        );
        assert!(vista.contains("No se pudo conectar"), "{vista}");
        // Y el motivo se ensena ahi mismo, que es donde el usuario esta mirando.
        assert!(vista.contains("retirale el permiso"), "{vista}");
        assert!(e.to_string().contains("token de refresco"), "{e}");
    }

    /// Y cuando sale bien, lo dice.
    #[tokio::test]
    async fn cuando_el_canje_sale_la_pestana_lo_confirma() {
        let (url, _recibido) = proveedor_falso(r#"{"refresh_token":"el-bueno"}"#).await;
        let oauth = oauth_falso(&url);

        let pagina = std::sync::Mutex::new(None);
        let abrir = |u: &str| -> Result<(), String> {
            let (r, canal) = navegador_que_guarda_la_respuesta(u);
            *pagina.lock().unwrap() = Some(canal);
            r
        };

        let c = conectar(&oauth, "id", "secreto", &abrir, None).await.unwrap();
        assert_eq!(c.refresh_token, "el-bueno");

        let canal = pagina.lock().unwrap().take().unwrap();
        let vista = canal.await.unwrap();
        assert!(vista.contains("Cuenta conectada"), "{vista}");
    }

    /// Un codigo que llega con otro `state` no se canjea.
    ///
    /// Es lo que impide que otro programa de esta maquina empuje un codigo suyo al puerto y
    /// consiga que MultiDrive conecte **su** cuenta en vez de la del usuario.
    #[tokio::test]
    async fn un_codigo_con_otro_state_se_rechaza() {
        let (url, _recibido) = proveedor_falso(r#"{"refresh_token":"no-deberia-usarse"}"#).await;
        let oauth = oauth_falso(&url);

        let impostor = |u: &str| -> Result<(), String> {
            let vuelta = consulta(u)
                .into_iter()
                .find(|(k, _)| k == "redirect_uri")
                .map(|(_, v)| v)
                .unwrap();
            tokio::spawn(async move {
                let destino = vuelta.trim_start_matches("http://");
                let mut socket = tokio::net::TcpStream::connect(destino).await.unwrap();
                socket
                    .write_all(b"GET /?code=el-mio&state=inventado HTTP/1.1\r\nHost: x\r\n\r\n")
                    .await
                    .unwrap();
                let mut basura = Vec::new();
                socket.read_to_end(&mut basura).await.ok();
            });
            Ok(())
        };

        let e = conectar(&oauth, "id", "secreto", &impostor, None)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("state"), "{e}");
    }

    /// El proveedor puede contestar sin token de refresco, y eso tiene un arreglo concreto
    /// que el mensaje tiene que decir. Google lo hace cuando la cuenta ya habia dado permiso.
    #[tokio::test]
    async fn sin_token_de_refresco_se_dice_que_hacer() {
        let (url, _recibido) = proveedor_falso(r#"{"access_token":"solo-este"}"#).await;
        let oauth = oauth_falso(&url);

        let e = conectar(&oauth, "id", "secreto", &navegador_que_acepta, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("retirale el permiso"), "{e}");
    }

    /// Una peticion suelta al puerto no tumba el flujo.
    ///
    /// El navegador pide cosas por su cuenta —`/favicon.ico` es la clasica— y cortar en la
    /// primera conexion dejaria colgado el flujo por una peticion que no es la del usuario.
    #[tokio::test]
    async fn una_peticion_que_no_es_la_buena_no_corta_el_flujo() {
        let (url, _recibido) = proveedor_falso(r#"{"refresh_token":"el-bueno"}"#).await;
        let oauth = oauth_falso(&url);

        let con_ruido = |u: &str| -> Result<(), String> {
            let params = consulta(u);
            let buscar = |c: &str| {
                params
                    .iter()
                    .find(|(k, _)| k == c)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            let vuelta = buscar("redirect_uri");
            let estado = buscar("state");
            tokio::spawn(async move {
                let destino = vuelta.trim_start_matches("http://").to_string();
                for peticion in [
                    "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
                    "GET /cualquier-cosa HTTP/1.1\r\nHost: x\r\n\r\n".to_string(),
                    format!("GET /?code=un-codigo&state={estado} HTTP/1.1\r\nHost: x\r\n\r\n"),
                ] {
                    let mut socket = tokio::net::TcpStream::connect(&destino).await.unwrap();
                    socket.write_all(peticion.as_bytes()).await.unwrap();
                    let mut basura = Vec::new();
                    socket.read_to_end(&mut basura).await.ok();
                }
            });
            Ok(())
        };

        let c = conectar(&oauth, "id", "secreto", &con_ruido, None).await.unwrap();
        assert_eq!(c.refresh_token, "el-bueno");
    }
}

#[cfg(test)]
mod tests_reintento_del_canje {
    use super::*;

    fn oauth_con_token(token: &str) -> Oauth {
        Oauth {
            autorizar: "http://127.0.0.1:1/autorizar",
            token: Box::leak(token.to_string().into_boxed_str()),
            ambito: "un.ambito",
            anfitrion: "127.0.0.1",
            cuota: "http://127.0.0.1:1/cuota",
            identidad: "http://127.0.0.1:1/identidad",
            campo_identidad: &["id"],
            extra: &[],
        }
    }

    /// Si la peticion no llega a salir, se repite, y se espera entre intento e intento.
    ///
    /// El reloj esta parado: lo que se mide es el tiempo **virtual**, que avanza solo cuando
    /// no queda nada por hacer. Asi el test comprueba las esperas sin tardarlas.
    #[tokio::test(start_paused = true)]
    async fn una_peticion_que_no_sale_se_repite() {
        // Un puerto sin nadie detras: la conexion se rechaza al instante, que es el fallo de
        // transporte mas limpio que se puede provocar sin tocar la red de la maquina.
        let oauth = oauth_con_token("http://127.0.0.1:1/token");
        let principio = tokio::time::Instant::now();

        let e = canjear(&oauth, "id", "secreto", "un-codigo", "http://127.0.0.1/", "v")
            .await
            .unwrap_err();

        let tardado = principio.elapsed();
        let suma: Duration = ESPERAS_ENTRE_CANJES.iter().sum();
        assert!(
            tardado >= suma,
            "tardo {tardado:?} y las esperas suman {suma:?}: no repitio las veces que debia"
        );
        // Y lo que se cuenta al final es el fallo, no un «reintentando» a medias.
        assert!(format!("{e}").contains("http"), "{e}");
    }

    /// Si el proveedor contesta, se le cree a la primera.
    ///
    /// Repetir un codigo que el proveedor ya vio no da otra respuesta: da la misma, y de paso
    /// hace esperar al usuario cuatro segundos largos para nada.
    #[tokio::test]
    async fn una_respuesta_del_proveedor_no_se_repite() {
        let servidor = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let puerto = servidor.local_addr().unwrap().port();
        let cuantas = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let suyas = std::sync::Arc::clone(&cuantas);
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = servidor.accept().await.unwrap();
                suyas.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut buf = vec![0u8; 8192];
                socket.read(&mut buf).await.ok();
                let cuerpo = "{\"error\":\"invalid_grant\"}";
                let cabecera = format!(
                    "HTTP/1.1 400 .\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    cuerpo.len()
                );
                socket.write_all(cabecera.as_bytes()).await.ok();
                socket.write_all(cuerpo.as_bytes()).await.ok();
                socket.shutdown().await.ok();
            }
        });

        let oauth = oauth_con_token(&format!("http://127.0.0.1:{puerto}/token"));
        let e = canjear(&oauth, "id", "secreto", "un-codigo", "http://127.0.0.1/", "v")
            .await
            .unwrap_err();

        assert_eq!(
            cuantas.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "se le pidio mas de una vez a un proveedor que ya habia contestado"
        );
        assert!(format!("{e}").contains("400"), "{e}");
    }
}

#[cfg(test)]
mod tests_pagina_de_vuelta {
    use super::*;

    /// En el escritorio no hay a donde volver, y la pagina no debe ofrecerlo: un boton
    /// «Volver a MultiDrive» que no lleva a ningun sitio es peor que no tenerlo.
    #[test]
    fn sin_destino_no_hay_boton_ni_guion() {
        let p = pagina("Cuenta conectada", "Ya puedes cerrar esta pestana.", None);
        assert!(!p.contains("Volver a MultiDrive"), "{p}");
        assert!(!p.contains("<script"), "{p}");
        assert!(!p.contains("<iframe"), "{p}");
    }

    /// Con destino van **las dos** cosas: el boton, que es el que funciona, y el intento
    /// automatico, que es el que a veces cuela. Sin el boton no hay salida ninguna.
    #[test]
    fn con_destino_van_el_salto_y_el_boton() {
        let p = pagina("Cuenta conectada", "Volviendo…", Some("multidrive://conectado"));
        assert!(p.contains("href=\"multidrive://conectado\""), "{p}");
        assert!(p.contains("Volver a MultiDrive"), "{p}");
        assert!(p.contains("<iframe"), "{p}");
    }

    /// El salto va por `iframe` y **no** por `location.replace`, y eso es la decision, no un
    /// detalle: un `replace` que el navegador rechaza se lleva por delante la pagina, y con
    /// ella el boton. Un iframe que falla no toca nada.
    #[test]
    fn el_salto_no_navega_la_pagina() {
        let p = pagina("x", "y", Some("multidrive://conectado"));
        assert!(!p.contains("location.replace"), "{p}");
        assert!(!p.contains("location.href"), "{p}");
    }

    /// Una comilla en la URL cerraria el atributo y lo de detras pasaria a ser marcado.
    #[test]
    fn una_comilla_en_la_url_no_rompe_el_atributo() {
        let p = pagina("x", "y", Some("multidrive://a\"onload=alert(1)"));
        assert!(!p.contains("\"onload="), "{p}");
        assert!(p.contains("&quot;onload="), "{p}");
    }

    /// El texto de la vuelta **manda pulsar**, y no dice que la cosa se este terminando sola.
    ///
    /// Esto no es cuidar el estilo. El texto anterior decia «Terminando en MultiDrive…»
    /// apoyandose en un salto automatico que el navegador bloquea: quien lo leia se quedaba
    /// mirando, con el boton que resolvia su problema delante y sin motivo para tocarlo.
    #[test]
    fn el_texto_de_la_vuelta_no_promete_un_salto_solo() {
        let d = VUELTA_DETALLE.to_lowercase();
        assert!(d.contains("pulsa"), "{VUELTA_DETALLE}");
        for promesa in ["terminando", "volviendo", "espera", "un momento", "automat"] {
            assert!(!d.contains(promesa), "«{promesa}» en «{VUELTA_DETALLE}»");
        }
        // Y el boton al que manda el texto tiene que existir en la pagina.
        let p = pagina(VUELTA_TITULO, VUELTA_DETALLE, Some("multidrive://conectado"));
        assert!(p.contains("Volver a MultiDrive"), "{p}");
    }

    /// Lo que cuenta el proveedor cuando falla tampoco se mete en HTML tal cual.
    #[test]
    fn el_motivo_del_proveedor_va_escapado() {
        let p = pagina("No se pudo conectar", "<script>malo</script>", None);
        assert!(!p.contains("<script>malo"), "{p}");
        assert!(p.contains("&lt;script&gt;malo"), "{p}");
    }
}
