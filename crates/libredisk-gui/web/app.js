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

  // El cupo es de la cuenta del proveedor, no del volumen: se puede ver con el volumen
  // cerrado, y es justo lo que hace falta para decidir si merece la pena crear uno.
  if (a.proveedores.length) refrescarEspacio($("espacio"));
  else $("espacio").replaceChildren(nota("No hay ningún proveedor configurado."));

  if (a.problema) {
    // Sin volumen no hay nada que abrir: pedir la frase seria pedir algo inutil. Pero si hay
    // proveedores configurados, si hay algo que hacer: crearlo.
    const creable = !a.hay_volumen && a.proveedores.length > 0;
    $("formulario").hidden = !a.hay_volumen;
    if (creable) {
      $("cabecera").textContent = "No hay ningún volumen todavía.";
      await ofrecerCreacion(a.proveedores);
    } else {
      aviso(a.problema, !a.hay_volumen);
    }
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
  // Cerrar con algo montado dejaria una unidad sirviendo un volumen que la ventana dice
  // haber cerrado. Se desmonta primero.
  await invoke("desmontar");
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
  await refrescarMontaje();
  await refrescarPendiente();
  await refrescarEspacio($("espacio-panel"));
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
    // La revision acaba de enumerar, que es lo unico que sabe cuanto ocupa el volumen en
    // cada proveedor. Las barras se repintan ahora para ensenar ese tramo.
    await refrescarEspacio($("espacio-panel"));
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



// ---------------------------------------------------------------------------------------
// Montar y desmontar
// ---------------------------------------------------------------------------------------

async function refrescarMontaje() {
  const m = await invoke("montaje");
  const caja = $("montaje");

  if (m.montado) {
    const boton = document.createElement("button");
    boton.className = "secundario";
    boton.textContent = "Desmontar";
    boton.addEventListener("click", async () => {
      boton.disabled = true;
      try {
        await invoke("desmontar");
        await refrescarMontaje();
        await refrescarPendiente();
      } finally {
        boton.disabled = false;
      }
    });
    const fila = document.createElement("div");
    fila.className = "enlinea";
    fila.append(boton);
    caja.replaceChildren(
      nota(`Montado en ${m.punto}, en ${m.escritura ? "lectura y escritura" : "solo lectura"}.`),
      fila,
    );
    return;
  }

  const campo = document.createElement("input");
  campo.type = "text";
  campo.id = "punto";
  campo.value = m.ejemplo;
  campo.spellcheck = false;

  const boton = document.createElement("button");
  boton.textContent = "Montar";

  const fila = document.createElement("div");
  fila.className = "enlinea";
  fila.append(campo, boton);

  const marca = document.createElement("input");
  marca.type = "checkbox";
  marca.id = "con-escritura";
  const etiqueta = document.createElement("label");
  etiqueta.htmlFor = "con-escritura";
  etiqueta.style.margin = "12px 0 0";
  etiqueta.append(marca, document.createTextNode(" Permitir escribir"));

  const error = nota("");
  error.hidden = true;

  boton.addEventListener("click", async () => {
    boton.disabled = true;
    error.hidden = true;
    try {
      await invoke("montar", { punto: campo.value, escritura: marca.checked });
      await refrescarMontaje();
      await refrescarPendiente();
    } catch (err) {
      error.textContent = String(err);
      error.hidden = false;
    } finally {
      boton.disabled = false;
    }
  });

  caja.replaceChildren(fila, etiqueta, error);
}

// ---------------------------------------------------------------------------------------
// Crear un volumen
// ---------------------------------------------------------------------------------------

async function ofrecerCreacion(proveedores) {
  $("creacion").hidden = false;
  $("sobre-proveedores").textContent =
    `Se creará sobre los ${proveedores.length} proveedores configurados: ` +
    `${proveedores.join(", ")}. Para usar otros, cámbialos en la configuración y reinicia.`;

  const opciones = await invoke("topologias", { proveedores: proveedores.length });
  const sel = $("topologia");
  sel.replaceChildren();
  for (const o of opciones) {
    const op = document.createElement("option");
    op.value = o.spec;
    op.textContent = o.spec;
    op.dataset.tolera = String(o.tolera);
    op.dataset.sobrecoste = o.sobrecoste.toFixed(2);
    op.dataset.aprovechado = Math.round(o.aprovechado * 100);
    op.dataset.sinRed = o.sin_red ? "1" : "";
    sel.append(op);
  }
  // La primera de la lista es la que más aguanta. Que el valor por defecto sea el prudente
  // importa: quien no sepa elegir se queda con el que menos sorpresas da.
  sel.selectedIndex = 0;
  mostrarPrecio();
}

function mostrarPrecio() {
  const op = $("topologia").selectedOptions[0];
  if (!op) return;
  const caja = $("precio");

  if (op.dataset.sinRed) {
    // Sin redundancia, el aviso no es un matiz del precio: es lo único que hay que saber
    // antes de elegirlo. Va en rojo y con todas las letras.
    caja.className = "aviso mal";
    caja.textContent =
      "NO aguanta ninguna caída: si un proveedor falla o te cierra la cuenta, pierdes el " +
      "volumen ENTERO, no una parte. A cambio aprovechas el 100 % del espacio y repartes " +
      "la lectura y la escritura entre todos.";
    return;
  }

  caja.className = "nota";
  caja.textContent =
    `Aguanta perder ${op.dataset.tolera} proveedor(es). ` +
    `Aprovechas el ${op.dataset.aprovechado}% del espacio: guardar 1 TB ocupa ` +
    `${op.dataset.sobrecoste} TB repartidos.`;
}

$("topologia").addEventListener("change", mostrarPrecio);

$("form-crear").addEventListener("submit", async (e) => {
  e.preventDefault();
  const a = $("frase-nueva");
  const b = $("frase-otra");
  if (a.value !== b.value) {
    // Sin recuperación posible, una errata al teclear la frase es un volumen perdido antes
    // de empezar. Por eso se pide dos veces.
    aviso("Las dos frases no coinciden.", true);
    b.select();
    return;
  }
  if (!a.value) return;

  $("boton-crear").disabled = true;
  aviso("");
  try {
    const r = await invoke("crear", { spec: $("topologia").value, frase: a.value });
    a.value = "";
    b.value = "";
    $("creacion").hidden = true;
    await mostrarPanel(r);
  } catch (err) {
    aviso(String(err), true);
  } finally {
    $("boton-crear").disabled = false;
  }
});

// Si esto falla no puede quedarse en "Comprobando...": un panel que no dice nada es peor que
// uno que dice que algo va mal.
arrancar().catch((err) => {
  $("cabecera").textContent = "No se pudo arrancar.";
  $("entrada").hidden = false;
  $("formulario").hidden = true;
  aviso(String(err?.message ?? err), true);
});

// ---------------------------------------------------------------------------------------
// Espacio por proveedor
// ---------------------------------------------------------------------------------------

/**
 * Pinta una barra por proveedor en `caja`.
 *
 * Las tres cifras son medidas distintas y aqui se mantienen separadas:
 * el total y lo ocupado los declara el proveedor y valen para **toda la cuenta**; lo que
 * ocupa el volumen solo se sabe despues de enumerar, y hasta entonces no se dibuja. Pintar
 * un cero seria afirmar que LibreDisk no ocupa nada ahi.
 */
async function refrescarEspacio(caja) {
  if (!caja) return;
  let proveedores;
  try {
    proveedores = await invoke("espacio");
  } catch (err) {
    caja.replaceChildren(nota(String(err)));
    return;
  }
  caja.replaceChildren(...proveedores.map(pintarProveedor));
}

function pintarProveedor(p) {
  const bloque = document.createElement("div");
  bloque.className = "proveedor";

  const cabeza = document.createElement("div");
  cabeza.className = "cabeza";
  const nombre = document.createElement("b");
  nombre.textContent = p.proveedor;
  const cifra = document.createElement("span");
  cabeza.append(nombre, cifra);
  bloque.append(cabeza);

  if (p.fallo) {
    // Un proveedor que no contesta no es un proveedor vacio: no lleva barra, lleva el
    // motivo. Una barra a cero aqui diria que esta libre del todo.
    cifra.textContent = "no se pudo consultar";
    bloque.append(nota(p.fallo));
    return bloque;
  }

  if (p.total === null) {
    // Un bucket S3 no tiene cupo: se paga por lo que ocupa. Dibujar una barra exigiria
    // inventarse un total.
    cifra.textContent = "sin límite declarado";
    bloque.append(
      nota(
        p.del_volumen === null
          ? (p.en_el_volumen === false
              ? "Este proveedor no declara ningún total, y no forma parte del volumen."
              : "Este proveedor no declara ningún total. Pulsa «Revisar» para medir lo que ocupa el volumen.")
          : `El volumen ocupa ${tamano(p.del_volumen)} aquí. Se paga por lo que ocupa, no hay cupo.`,
      ),
    );
    return bloque;
  }

  const usado = p.usado ?? 0;
  cifra.textContent = `${tamano(usado)} de ${tamano(p.total)}`;

  const barra = document.createElement("div");
  barra.className = "barra";
  const fraccion = p.total > 0 ? usado / p.total : 0;
  if (fraccion >= 0.95) barra.classList.add("llena");
  else if (fraccion >= 0.85) barra.classList.add("apurada");

  // El tramo de LibreDisk nunca se pinta mayor que lo que el proveedor dice que hay
  // ocupado. Las dos cifras vienen de sitios distintos y pueden no cuadrar --una medida es
  // de hace un minuto y la otra de hace media hora-- y en ese caso manda la del proveedor.
  const nuestro = Math.min(p.del_volumen ?? 0, usado);
  bloque.append(barra);
  for (const [clase, bytes] of [["nuestro", nuestro], ["ajeno", usado - nuestro]]) {
    if (bytes <= 0) continue;
    const tramo = document.createElement("div");
    tramo.className = clase;
    tramo.style.width = `${(bytes / p.total) * 100}%`;
    barra.append(tramo);
  }

  const leyenda = document.createElement("div");
  leyenda.className = "leyenda";
  const partes =
    p.del_volumen === null
      ? [["", `${tamano(usado)} ocupados en la cuenta`], ["libre", `${tamano(p.total - usado)} libres`]]
      : [
          ["nuestro", `${tamano(nuestro)} este volumen`],
          ["", `${tamano(usado - nuestro)} otras cosas de la cuenta`],
          ["libre", `${tamano(p.total - usado)} libres`],
        ];
  for (const [clase, texto] of partes) {
    const s = document.createElement("span");
    s.className = clase;
    s.textContent = texto;
    leyenda.append(s);
  }
  bloque.append(leyenda);

  if (p.en_el_volumen === false) {
    // Configurado, pero fuera del volumen. Mandarlo a «Revisar» seria mandarlo a enumerar
    // durante minutos para que no cambiara nada.
    bloque.append(nota("Este proveedor no forma parte del volumen: aquí no guarda nada."));
  } else if (p.del_volumen === null) {
    bloque.append(
      nota("Cuánto de esto es del volumen no se sabe hasta enumerar: pulsa «Revisar»."),
    );
  }
  return bloque;
}
