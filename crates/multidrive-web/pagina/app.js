// La pagina de la version web: iniciar sesion en las cuentas y llevarle los tokens a la Sesion.
//
// Todo lo que toca el volumen --cabecera, maleta, descifrado, reconstruir ficheros-- lo hace
// el WebAssembly, con el mismo codigo que el escritorio. Aqui solo hay interfaz y OAuth.
//
// # Como se entra en cada cuenta, y por que asi
//
// Sin secretos y sin tokens que duren. El navegador no puede guardar un secreto, y un token de
// refresco en el almacenamiento de la pagina seria acceso permanente al alcance de cualquier
// fallo de la pagina.
//
// - Google: flujo implicito. La ventana de Google devuelve un token de acceso de una hora y
//   nada mas. No se carga ningun script de Google en esta pagina: la ventana es suya, la
//   pagina es nuestra.
// - Microsoft: codigo con PKCE contra la plataforma SPA de la aplicacion. Es el unico flujo
//   que Microsoft deja canjear desde un navegador; el token de refresco que da caduca en un
//   dia y aqui ni se usa.
//
// La ventana vuelve a esta misma pagina, que al ver que es la ventana de vuelta le pasa el
// resultado a la pestana principal por un BroadcastChannel y se cierra. Por el canal y no por
// `window.opener` porque las paginas de acceso de Google cortan el `opener` al navegar.

import iniciar, { Sesion } from "./pkg/multidrive_web.js";

const GOOGLE_CLIENTE = "14801824548-684l8s1nfdd6kovmickr7jnj8shcfg5k.apps.googleusercontent.com";
const MICROSOFT_CLIENTE = "5eee79de-1b07-4fec-ae03-a4d3bc3b94f2";
// `drive.file` es el unico ambito no sensible de Drive, y con el se ven justo los ficheros
// que creo MultiDrive. En OneDrive basta con leer.
const GOOGLE_AMBITO = "https://www.googleapis.com/auth/drive.file";
const MICROSOFT_AMBITO = "Files.Read";

// Tiene que coincidir letra por letra con la registrada en Google y en Azure.
const VUELTA = `${location.origin}/app/`;
const CANAL = "multidrive-acceso";

// ---------------------------------------------------------------------------------------------
// La ventana de vuelta
// ---------------------------------------------------------------------------------------------

function soyLaVentanaDeVuelta() {
  const hash = new URLSearchParams(location.hash.slice(1));
  const query = new URLSearchParams(location.search);
  const state = hash.get("state") || query.get("state");
  if (!state) return false;

  const mensaje = {
    state,
    token: hash.get("access_token"),
    code: query.get("code"),
    error: hash.get("error_description") || hash.get("error")
      || query.get("error_description") || query.get("error"),
  };
  // Se quita de la barra de direcciones antes de nada: un token en la URL acaba en el
  // historial.
  history.replaceState(null, "", location.pathname);
  const canal = new BroadcastChannel(CANAL);
  canal.postMessage(mensaje);
  canal.close();
  document.body.textContent = "Listo. Puedes cerrar esta ventana.";
  window.close();
  return true;
}

// ---------------------------------------------------------------------------------------------
// OAuth
// ---------------------------------------------------------------------------------------------

function aleatorio(bytes = 32) {
  const b = crypto.getRandomValues(new Uint8Array(bytes));
  return base64url(b);
}

function base64url(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/// Abre la ventana del proveedor y espera a que vuelva. Devuelve lo que traiga la URL de vuelta.
function ventana(url, state) {
  return new Promise((resolver, rechazar) => {
    const canal = new BroadcastChannel(CANAL);
    const v = window.open(url, "multidrive-acceso", "popup,width=520,height=680");
    if (!v) {
      canal.close();
      rechazar(new Error("el navegador bloqueó la ventana de acceso; permite las ventanas emergentes de esta página"));
      return;
    }
    canal.onmessage = (e) => {
      // Solo la respuesta a **esta** peticion. Otra cosa por el canal no se cree.
      if (e.data?.state !== state) return;
      canal.close();
      if (e.data.error) rechazar(new Error(e.data.error));
      else resolver(e.data);
    };
  });
}

async function entrarGoogle() {
  const state = aleatorio();
  const p = new URLSearchParams({
    client_id: GOOGLE_CLIENTE,
    redirect_uri: VUELTA,
    response_type: "token",
    scope: GOOGLE_AMBITO,
    state,
    // Elegir cuenta cada vez: con varias de Google en el navegador, entrar en silencio en la
    // que no es daria «no hay volumen» sin explicar por que.
    prompt: "select_account",
    include_granted_scopes: "true",
  });
  const r = await ventana(`https://accounts.google.com/o/oauth2/v2/auth?${p}`, state);
  if (!r.token) throw new Error("Google no devolvió ningún token");
  return r.token;
}

async function entrarMicrosoft() {
  const state = aleatorio();
  const verificador = aleatorio(48);
  const reto = base64url(new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verificador)),
  ));
  const p = new URLSearchParams({
    client_id: MICROSOFT_CLIENTE,
    redirect_uri: VUELTA,
    response_type: "code",
    response_mode: "query",
    scope: MICROSOFT_AMBITO,
    state,
    code_challenge: reto,
    code_challenge_method: "S256",
    prompt: "select_account",
  });
  const r = await ventana(
    `https://login.microsoftonline.com/common/oauth2/v2.0/authorize?${p}`, state);
  if (!r.code) throw new Error("Microsoft no devolvió ningún código");

  const canje = await fetch("https://login.microsoftonline.com/common/oauth2/v2.0/token", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      client_id: MICROSOFT_CLIENTE,
      grant_type: "authorization_code",
      code: r.code,
      redirect_uri: VUELTA,
      code_verifier: verificador,
      scope: MICROSOFT_AMBITO,
    }),
  });
  const t = await canje.json();
  if (!canje.ok || !t.access_token) {
    throw new Error(t.error_description?.split("\r\n")[0] || "Microsoft no dio el token");
  }
  return t.access_token;
}

const entrar = { gdrive: entrarGoogle, onedrive: entrarMicrosoft };
const nombreDeTipo = { gdrive: "Google Drive", onedrive: "OneDrive" };

// ---------------------------------------------------------------------------------------------
// La pagina
// ---------------------------------------------------------------------------------------------

const $ = (id) => document.getElementById(id);
let sesion;
let cabecera;          // { topologia, cuentas: [{nombre, tipo, conectada}] }
let raices = {};       // ranura -> carpeta, de la maleta
let frase = "";
let ruta = [];

function estado(texto, error = false) {
  const e = $("estado");
  e.textContent = texto;
  e.classList.toggle("error", error);
}

function mostrar(id) {
  for (const s of document.querySelectorAll("main > section")) s.hidden = s.id !== id;
}

async function ocupado(texto, trabajo) {
  estado(texto);
  document.body.classList.add("ocupado");
  try {
    return await trabajo();
  } catch (e) {
    estado(String(e?.message ?? e), true);
    throw e;
  } finally {
    document.body.classList.remove("ocupado");
  }
}

/// Paso 1: la primera cuenta, con un nombre provisional hasta saber cual es de las del volumen.
async function primeraCuenta(tipo) {
  const token = await ocupado(`Entrando en ${nombreDeTipo[tipo]}…`, () => entrar[tipo]());
  const raiz = $("raiz").value.trim();
  const provisional = `${tipo}`;
  sesion.cuenta(provisional, raiz, token);

  cabecera = JSON.parse(await ocupado("Buscando el volumen…", () => sesion.cabecera()));
  const candidatas = cabecera.cuentas.filter((c) => c.tipo === tipo);
  if (candidatas.length === 0) {
    sesion.quitar(provisional);
    estado(`El volumen no tiene ninguna cuenta de ${nombreDeTipo[tipo]}. Entra con una de las suyas.`, true);
    return;
  }
  if (candidatas.length === 1) {
    asignar(provisional, candidatas[0].nombre, raiz, token);
    return;
  }

  // Varias del mismo tipo: que lo diga quien sabe en que cuenta ha entrado.
  const cual = $("cual");
  cual.replaceChildren(...candidatas.map((c) => {
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = c.nombre;
    b.onclick = () => asignar(provisional, c.nombre, raiz, token);
    return b;
  }));
  mostrar("paso-cual");
  estado("");
}

function asignar(provisional, nombre, raiz, token) {
  if (provisional !== nombre) {
    sesion.quitar(provisional);
    sesion.cuenta(nombre, raiz, token);
  }
  for (const c of cabecera.cuentas) if (c.nombre === nombre) c.conectada = true;
  const n = cabecera.cuentas.length;
  $("resumen").textContent = `Volumen ${cabecera.topologia} repartido en ${n} cuenta${n === 1 ? "" : "s"}: `
    + cabecera.cuentas.map((c) => c.nombre).join(", ") + ".";
  mostrar("paso-frase");
  estado("");
  $("frase").focus();
}

/// Paso 2: la frase. Abre la maleta para saber las carpetas de las demas, y si ya estan todas,
/// el volumen.
async function conFrase(e) {
  e.preventDefault();
  frase = $("frase").value;
  // Argon2 congela la pestana unos segundos: que se vea por que.
  raices = JSON.parse(await ocupado("Comprobando la frase… (unos segundos)", () => sesion.maleta(frase)));
  await siguiente();
}

async function siguiente() {
  const faltan = cabecera.cuentas.filter((c) => !c.conectada);
  if (faltan.length === 0) {
    await ocupado("Abriendo el volumen… (unos segundos)", () => sesion.abrir(frase));
    $("frase").value = "";
    ruta = [];
    mostrar("paso-volumen");
    await pintar();
    return;
  }

  const lista = $("cuentas");
  lista.replaceChildren(...cabecera.cuentas.map((c) => {
    const li = document.createElement("li");
    const nombre = document.createElement("span");
    nombre.textContent = `${c.nombre} · ${nombreDeTipo[c.tipo] ?? "tipo que la web no lee"}`;
    li.append(nombre);
    if (c.conectada) {
      const ok = document.createElement("span");
      ok.className = "hecho";
      ok.textContent = "conectada";
      li.append(ok);
    } else if (!c.tipo) {
      const no = document.createElement("span");
      no.className = "no";
      no.textContent = "solo desde la aplicación";
      li.append(no);
    } else {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = "Entrar";
      b.onclick = async () => {
        const token = await ocupado(`Entrando en ${c.nombre}…`, () => entrar[c.tipo]());
        sesion.cuenta(c.nombre, raices[c.nombre] ?? "", token);
        c.conectada = true;
        await siguiente();
      };
      li.append(b);
    }
    return li;
  }));
  mostrar("paso-resto");
  estado(faltan.some((c) => !c.tipo)
    ? "Este volumen tiene cuentas que el navegador no sabe leer, así que desde aquí no se puede abrir."
    : "");
}

// ---------------------------------------------------------------------------------------------
// Navegar
// ---------------------------------------------------------------------------------------------

function tamano(n) {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${i === 0 ? n : n.toFixed(1).replace(".", ",")} ${u[i]}`;
}

function fecha(s) {
  if (s == null) return "";
  return new Date(s * 1000).toLocaleDateString("es", { day: "numeric", month: "short", year: "numeric" });
}

async function pintar() {
  const entradas = JSON.parse(await ocupado("Leyendo…", () => sesion.listar("/" + ruta.join("/"))));
  estado("");

  const migas = $("migas");
  const raiz = document.createElement("button");
  raiz.type = "button";
  raiz.textContent = "Volumen";
  raiz.onclick = () => { ruta = []; pintar(); };
  migas.replaceChildren(raiz, ...ruta.flatMap((seg, i) => {
    const sep = document.createElement("span");
    sep.textContent = "/";
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = seg;
    b.onclick = () => { ruta = ruta.slice(0, i + 1); pintar(); };
    return [sep, b];
  }));

  // Carpetas primero, y luego por nombre como lo ordenaria una persona.
  entradas.sort((a, b) => (b.dir - a.dir) || a.nombre.localeCompare(b.nombre, "es", { numeric: true }));
  const lista = $("lista");
  if (entradas.length === 0) {
    const li = document.createElement("li");
    li.className = "vacio";
    li.textContent = "Esta carpeta está vacía.";
    lista.replaceChildren(li);
    return;
  }
  lista.replaceChildren(...entradas.map((e) => {
    const li = document.createElement("li");
    const b = document.createElement("button");
    b.type = "button";
    b.className = e.dir ? "dir" : "fichero";
    const nombre = document.createElement("span");
    nombre.className = "nombre";
    nombre.textContent = e.nombre;
    const meta = document.createElement("span");
    meta.className = "meta";
    meta.textContent = e.dir
      ? `${e.len} elemento${e.len === 1 ? "" : "s"}`
      : [tamano(e.len), fecha(e.modificado)].filter(Boolean).join(" · ");
    b.append(nombre, meta);
    b.onclick = e.dir
      ? () => { ruta.push(e.nombre); pintar(); }
      : () => descargar(e.nombre, e.len);
    li.append(b);
    return li;
  }));
}

async function descargar(nombre, len) {
  const camino = "/" + [...ruta, nombre].join("/");
  const bytes = await ocupado(`Descargando ${nombre} (${tamano(len)})…`, () => sesion.leer(camino));
  const url = URL.createObjectURL(new Blob([bytes]));
  const a = document.createElement("a");
  a.href = url;
  a.download = nombre;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
  estado(`${nombre} descargado.`);
}

function cerrar() {
  // Una sesion nueva y vacia: el volumen, los tokens y la clave se van con la vieja.
  sesion.free();
  sesion = new Sesion();
  cabecera = undefined;
  raices = {};
  frase = "";
  ruta = [];
  mostrar("paso-entrar");
  estado("Volumen cerrado.");
}

// ---------------------------------------------------------------------------------------------

if (!soyLaVentanaDeVuelta()) {
  await iniciar();
  sesion = new Sesion();
  for (const b of document.querySelectorAll("[data-entrar]")) {
    b.onclick = () => primeraCuenta(b.dataset.entrar).catch(() => {});
  }
  $("form-frase").onsubmit = (e) => conFrase(e).catch(() => {});
  $("cerrar").onclick = cerrar;
}
