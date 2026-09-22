// Interfaz de LibreDisk. Sin dependencias: lo que hay aqui es todo lo que se ejecuta.
const invoke = window.__TAURI__.core.invoke;

const $ = (id) => document.getElementById(id);

/** Rellena una `<dl>` a partir de pares, escapando por construccion. */
function definiciones(dl, pares) {
  dl.replaceChildren();
  for (const [clave, valor] of pares) {
    const dt = document.createElement("dt");
    dt.textContent = clave;
    const dd = document.createElement("dd");
    dd.textContent = valor;
    dl.append(dt, dd);
  }
}

function aviso(texto, grave = false) {
  const el = $("aviso");
  el.textContent = texto;
  el.classList.toggle("mal", grave);
  el.hidden = !texto;
}

function tamano(bytes) {
  const u = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0, n = bytes;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${i === 0 ? n : n.toFixed(1)} ${u[i]}`;
}

async function arrancar() {
  const a = await invoke("arranque");

  if (a.abierto) return mostrarPanel();

  $("entrada").hidden = false;
  $("cabecera").textContent = a.hay_volumen
    ? "Hay un volumen. Hace falta la frase de paso para abrirlo."
    : "Un volumen sobre varias nubes, cifrado en el cliente.";

  const pares = [["configurados", a.proveedores.join(", ") || "ninguno"]];
  if (a.hay_volumen) {
    pares.push(["topologia", a.layout]);
    pares.push(["del volumen", a.del_volumen.join(", ")]);
  }
  definiciones($("previo"), pares);

  if (a.problema) {
    aviso(a.problema, !a.hay_volumen);
    // Sin volumen no hay nada que abrir: pedir la frase seria pedir algo inutil.
    $("formulario").hidden = !a.hay_volumen;
  }
}

$("formulario").addEventListener("submit", async (e) => {
  e.preventDefault();
  const campo = $("frase");
  $("abrir").disabled = true;
  aviso("");
  try {
    const r = await invoke("abrir", { frase: campo.value });
    // Se borra en cuanto deja de hacer falta, aunque el proceso sea el mismo: un campo con
    // la frase a la vista es una captura de pantalla de mas.
    campo.value = "";
    await mostrarPanel(r);
  } catch (err) {
    aviso(String(err), true);
    campo.select();
  } finally {
    $("abrir").disabled = false;
  }
});

$("cerrar").addEventListener("click", async () => {
  await invoke("cerrar");
  $("panel").hidden = true;
  $("salud").replaceChildren(nota("Sin revisar todavía."));
  await arrancar();
});

function nota(texto) {
  const p = document.createElement("p");
  p.className = "nota";
  p.textContent = texto;
  return p;
}

async function mostrarPanel(ya) {
  $("entrada").hidden = true;
  $("panel").hidden = false;
  $("cabecera").textContent = "Volumen abierto.";

  // Al arrancar con el volumen ya abierto no hay resumen a mano: se pide.
  const r = ya ?? (await invoke("resumen"));
  rellenarResumen(r);
  await refrescarPendiente();
}

function rellenarResumen(r) {
  definiciones($("resumen"), [
    ["topologia", r.layout],
    ["proveedores", r.proveedores.join(", ")],
    ["tolera perder", `${r.tolera} proveedor(es)`],
    ["sobrecoste", `${r.sobrecoste.toFixed(2)}x`],
    ["reparto", String(r.reparto)],
    ["generacion", String(r.generacion)],
  ]);
}

$("revisar").addEventListener("click", async () => {
  const boton = $("revisar");
  boton.disabled = true;
  $("salud").replaceChildren(nota("Revisando… enumera cada proveedor, puede tardar."));
  try {
    const s = await invoke("salud");
    pintarSalud(s);
  } catch (err) {
    $("salud").replaceChildren(nota(String(err)));
  } finally {
    boton.disabled = false;
  }
});

function pintarSalud(s) {
  const caja = document.createElement("div");
  caja.className = "cuentas";
  const cuentas = [
    ["completos", s.completos, "ok"],
    ["degradados", s.degradados, s.degradados ? "aviso2" : ""],
    ["perdidos", s.perdidos, s.perdidos ? "mal" : ""],
  ];
  for (const [nombre, n, clase] of cuentas) {
    const d = document.createElement("div");
    d.className = `cuenta ${clase}`;
    const b = document.createElement("b");
    b.textContent = String(n);
    d.append(b, document.createTextNode(nombre));
    caja.append(d);
  }

  const partes = [caja];
  // Con proveedores sin enumerar, las cifras son un suelo pesimista y no una medida. Decirlo
  // importa mas que las cifras: dar por perdido un trozo por un proveedor que no contesto
  // llevaria a reparar lo que no esta roto.
  if (s.sin_enumerar.length) {
    partes.push(
      nota("Estas cifras son un mínimo, no una medida: hay proveedores que no se dejaron enumerar."),
      lista(s.sin_enumerar),
    );
  }
  if (s.ilegibles.length) {
    partes.push(nota("Rutas que no se pudieron leer:"), lista(s.ilegibles));
  }
  $("salud").replaceChildren(...partes);
}

function lista(items) {
  const ul = document.createElement("ul");
  ul.className = "lista";
  for (const t of items.slice(0, 8)) {
    const li = document.createElement("li");
    li.textContent = t;
    ul.append(li);
  }
  return ul;
}

async function refrescarPendiente() {
  try {
    const p = await invoke("pendiente");
    const caja = $("pendiente");
    if (p.a_salvo) {
      caja.replaceChildren(nota("Nada pendiente: todo lo escrito está en los proveedores."));
    } else {
      caja.replaceChildren(
        nota(`${p.trozos} trozo(s), ${tamano(p.bytes)} SOLO EN ESTA MÁQUINA.`),
        nota("Hasta que suba, perder este equipo pierde eso. `libredisk sync` lo sube."),
      );
    }
  } catch {
    // Sin volumen abierto no hay cola que consultar; no es un fallo que anunciar.
  }
}

// Si esto falla no puede quedarse en "Comprobando...": un panel que no dice nada es peor que
// uno que dice que algo va mal.
arrancar().catch((err) => {
  $("cabecera").textContent = "No se pudo arrancar.";
  $("entrada").hidden = false;
  $("formulario").hidden = true;
  aviso(String(err?.message ?? err), true);
});
