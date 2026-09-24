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

  // La visibilidad se decide **siempre**, no solo cuando hay un problema. Antes se fijaba
  // dentro de esa rama, asi que quien creaba un volumen y volvia a esta pantalla se
  // encontraba con que decia «hace falta la frase de paso» y no habia donde escribirla: el
  // formulario seguia oculto de cuando no habia volumen que abrir.
  $("formulario").hidden = !a.hay_volumen;
  $("retirar").hidden = !a.hay_volumen;
  $("confirmar-borrado").hidden = true;
  $("confirmar-borrado").replaceChildren();

  if (a.problema) {
    // Sin volumen no hay nada que abrir: pedir la frase seria pedir algo inutil. Pero si hay
    // proveedores configurados, si hay algo que hacer: crearlo.
    if (!a.hay_volumen && a.proveedores.length > 0) {
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

// ---------------------------------------------------------------------------------------
// Añadir proveedores
// ---------------------------------------------------------------------------------------

/**
 * Lo tecleado, por clave de campo. Vive fuera del DOM para que cambiar de tipo y volver no
 * borre lo que ya se había escrito: quien pega un token de refresco de 200 caracteres y
 * luego mira otra pestaña no debería tener que pegarlo otra vez.
 */
const tecleado = new Map();
/** Tipos cuya cuenta ya se ha conectado por navegador en esta sesion. */
const conectado = new Set();
let tipos = [];

async function prepararAlta() {
  try {
    tipos = await invoke("tipos_de_proveedor");
  } catch (err) {
    $("alta-cuerpo").replaceChildren(nota(String(err)));
    return;
  }
  const sel = $("tipo-proveedor");
  // Se conserva el tipo elegido al repintar. Tras guardar una cuenta de Drive, lo normal es
  // querer añadir otra de Drive; volver a «Carpeta local» sería mandar al usuario a
  // recorrer la lista otra vez.
  const antes = sel.value;
  sel.replaceChildren();
  for (const t of tipos) {
    const op = document.createElement("option");
    op.value = t.clave;
    op.textContent = t.etiqueta;
    sel.append(op);
  }
  if (tipos.some((t) => t.clave === antes)) sel.value = antes;
  pintarFormularioProveedor();
}

function tipoElegido() {
  return tipos.find((t) => t.clave === $("tipo-proveedor").value);
}

function pintarFormularioProveedor() {
  const t = tipoElegido();
  const caja = $("campos-proveedor");
  if (!t) return;

  tecleado.clear();
  conectado.delete(t.clave);
  const partes = [nota(t.explicacion)];

  if (t.hueco) {
    // El nombre con el que el volumen lo va a conocer para siempre. Ver «gdrive2» es lo que
    // le dice a alguien que su primera cuenta de Drive sigue ahí y no se va a pisar.
    const yaHay = t.ocupadas ?? 0;
    partes.push(
      nota(
        yaHay === 0
          ? `Se guardará con el nombre «${t.hueco}».`
          : `Ya tienes ${yaHay} de este tipo. Esta se guardará como «${t.hueco}», ` +
            `aparte de ${t.nombres.join(", ")}.`,
      ),
    );
    if (t.oauth) {
      // Que se puede repetir el tipo no es evidente: el formulario se ve igual, y quien no
      // lo sepa no lo intenta.
      partes.push(
        nota(
          "Puedes añadir varias cuentas del mismo servicio: el navegador te dejará elegir " +
            "cuál. Repetir la misma cuenta no sirve —se vería como dos proveedores pero " +
            "caería a la vez—, así que se comprueba y se rechaza.",
        ),
      );
    }
  } else {
    partes.push(nota("No quedan ranuras libres de este tipo."));
  }

  for (const c of t.campos) {
    // Lo que identifica a la aplicación no se pide si el programa ya lo trae dentro:
    // pedirle a alguien el ID de una aplicación que no es suya es pedirle un dato que no
    // tiene.
    if (c.aplicacion && t.aplicacion_propia) continue;
    // Los que tienen un valor por defecto que sirve no se preguntan: la carpeta dentro de
    // una cuenta de nube es siempre la misma, y da igual cómo se llame porque dentro solo
    // hay blobs opacos que nadie va a abrir desde la web del proveedor.
    if (c.avanzado) continue;
    if (c.navegador) {
      // No es una caja de texto: es un botón. Nadie debería teclear un token de 200
      // caracteres, y el token tampoco pasa por esta página: se queda en el proceso.
      const boton = document.createElement("button");
      boton.type = "button";
      // Sin `secundario`: para un proveedor de nube, este botón **es** la acción. Conectar
      // la cuenta es configurarlo entero, así que no hay nada más que rellenar después.
      boton.textContent = `Conectar con ${t.etiqueta}`;
      const aviso = nota("");
      aviso.hidden = true;
      boton.addEventListener("click", async () => {
        boton.disabled = true;
        aviso.hidden = false;
        aviso.className = "nota";
        let parar = contando(aviso, "Abriendo el navegador. Entra en tu cuenta y da permiso…");
        try {
          await invoke("conectar_nube", { tipo: t.clave, campos: campos() });
          conectado.add(t.clave);
          // Y se guarda sin pedir otro clic. Conectar la cuenta es lo que configura el
          // proveedor; obligar a pulsar un segundo botón era trabajo que no aportaba nada.
          // `guardar_proveedor` sigue probando antes de escribir, como siempre.
          parar();
          parar = contando(
            aviso,
            "Conectada. Comprobando contra la nube que escribe, lee y borra; puede tardar medio minuto…",
          );
          await guardado(
            await invoke("guardar_proveedor", { tipo: t.clave, campos: campos() }),
          );
        } catch (err) {
          // En rojo y con todas las letras: una línea gris debajo de un botón se pasa por
          // alto, y entonces parece que no ha pasado nada cuando lo que ha pasado es que
          // ha fallado.
          aviso.className = "aviso mal";
          aviso.textContent = String(err);
        } finally {
          parar();
          boton.disabled = false;
        }
      });
      partes.push(boton, aviso);
      if (c.ayuda) partes.push(nota(c.ayuda));
      continue;
    }

    const etiqueta = document.createElement("label");
    etiqueta.htmlFor = `campo-${c.clave}`;
    etiqueta.textContent = c.etiqueta + (c.obligatorio ? "" : " (opcional)");

    const campo = document.createElement("input");
    campo.id = `campo-${c.clave}`;
    // Un secreto sin tapar acaba en una captura de pantalla. La marca viene del catálogo,
    // que es también quien decide qué se imprime y qué no en la línea de órdenes.
    campo.type = c.secreto ? "password" : "text";
    campo.autocomplete = "off";
    campo.spellcheck = false;
    campo.placeholder = c.ejemplo;
    campo.addEventListener("input", () => {
      tecleado.set(c.clave, campo.value);
      $("resultado-proveedor").hidden = true;
    });

    partes.push(etiqueta, campo);
    if (c.ayuda) partes.push(nota(c.ayuda));
  }

  caja.replaceChildren(...partes);
  // Los botones de probar y guardar solo tienen sentido en los tipos que se rellenan a
  // mano. En los de nube, el botón de conectar ya lo hace todo.
  $("botones-manual").hidden = t.oauth;
  $("resultado-proveedor").hidden = true;
}

function campos() {
  const out = {};
  for (const [k, v] of tecleado) if (v.trim()) out[k] = v;
  return out;
}

/**
 * Enseña un mensaje con los segundos corriendo, y devuelve cómo pararlo.
 *
 * Existe porque una línea de texto quieta no distingue «trabajando» de «colgado». Contra una
 * nube, guardar un proveedor son del orden de veinte o treinta segundos —escribir, leer,
 * borrar, y de paso mirar si ya hay un volumen—, y durante todo ese rato la pantalla parecía
 * muerta. Con el contador se ve que sigue.
 */
function contando(el, texto) {
  const desde = Date.now();
  const pintar = () => {
    const s = Math.round((Date.now() - desde) / 1000);
    el.textContent = s > 2 ? `${texto} (${s} s)` : texto;
  };
  pintar();
  const id = setInterval(pintar, 1000);
  return () => clearInterval(id);
}

function resultado(texto, mal = false) {
  const caja = $("resultado-proveedor");
  caja.className = mal ? "aviso mal" : "nota";
  caja.textContent = texto;
  caja.hidden = false;
}

$("tipo-proveedor").addEventListener("change", pintarFormularioProveedor);


$("guardar-proveedor").addEventListener("click", async () => {
  const boton = $("guardar-proveedor");
  boton.disabled = true;
  const parar = contando($("resultado-proveedor"), "Comprobando contra el proveedor…");
  $("resultado-proveedor").hidden = false;
  try {
    await guardado(
      await invoke("guardar_proveedor", {
        tipo: $("tipo-proveedor").value,
        campos: campos(),
      }),
    );
  } catch (err) {
    resultado(String(err), true);
  } finally {
    boton.disabled = false;
  }
});

// Abrir y cerrar el alta. El botón de fuera dice lo que hace —«Añadir un proveedor»— y
// desaparece mientras el formulario está abierto, porque entonces ya no hay nada que añadir
// a lo que invitar: lo que hay es un formulario a medio rellenar.
$("abrir-alta").addEventListener("click", async () => {
  $("abrir-alta").hidden = true;
  $("alta-cuerpo").hidden = false;
  // Se prepara al abrirlo y no al arrancar: consultar qué ranura queda libre lee la
  // configuración, y no hay por qué pagarlo si nadie va a añadir nada.
  await prepararAlta();
});

$("cerrar-alta").addEventListener("click", () => {
  $("alta-cuerpo").hidden = true;
  $("abrir-alta").hidden = false;
  // Lo tecleado se suelta al cerrar: ahí puede haber un secreto de aplicación, y no tiene
  // por qué seguir en memoria porque alguien haya dejado el formulario a medias.
  tecleado.clear();
});
/**
 * Lo que pasa después de que un proveedor quede guardado, venga del botón que venga.
 *
 * Refresca la pantalla y **deja el formulario listo para el siguiente**: con la ranura que
 * toca ahora («gdrive1» después de guardar «gdrive») y la cuenta ya contada. Añadir varias
 * cuentas seguidas es el caso normal, no la excepción.
 *
 * El `arrancar()` es lo que actualiza la lista de proveedores y las barras de espacio. Sin
 * él, el proveedor está guardado pero la pantalla sigue diciendo «ninguno», y lo que el
 * usuario ve no es lo que hay.
 */
async function guardado(g) {
  let texto = `Guardado como «${g.nombre}». Ya aparece en la lista de arriba.`;
  if (g.hay_volumen) {
    // Configurar no es meterlo en el volumen, y son cosas de coste muy distinto. Decir aquí
    // que ya está dentro sería la mentira más cara de esta pantalla.
    texto +=
      "\n\nOJO: ya hay un volumen, y esto NO lo mete dentro. Para eso hay que mover " +
      "fragmentos, y hay que elegir para qué:\n" +
      "  libredisk providers add --mas-seguridad   (aguanta una caída más; barato)\n" +
      "  libredisk providers add --mas-espacio     (más capacidad; reescribe el volumen entero)";
  }
  await arrancar();
  // Después de repintar, porque repintar borra el resultado anterior.
  await prepararAlta();
  resultado(texto);
}

// ---------------------------------------------------------------------------------------
// Retirar el volumen
// ---------------------------------------------------------------------------------------

$("pedir-borrar").addEventListener("click", async () => {
  const caja = $("confirmar-borrado");
  const boton = $("pedir-borrar");
  boton.disabled = true;
  caja.hidden = false;
  const parar = contando(
    caja.appendChild(nota("")),
    "Mirando qué hay que borrar; enumera cada proveedor y puede tardar…",
  );
  try {
    const v = await invoke("volumen_a_borrar");
    parar();
    pedirConfirmacion(v);
  } catch (err) {
    parar();
    caja.replaceChildren();
    const mal = nota(String(err));
    mal.className = "aviso mal";
    caja.append(mal);
  } finally {
    boton.disabled = false;
  }
});

/**
 * Enseña qué se va a borrar y pide escribirlo para confirmar.
 *
 * Se pide **escribir los nombres de los proveedores** y no pulsar «sí»: obliga a mirar la
 * lista. Un botón que solo hay que pulsar se pulsa sin leer, y esto no tiene vuelta atrás.
 */
function pedirConfirmacion(v) {
  const caja = $("confirmar-borrado");
  const partes = [];

  const resumen = document.createElement("dl");
  definiciones(resumen, [
    ["topologia", v.layout],
    ["proveedores", v.proveedores.join(", ")],
    ["fragmentos", `${v.fragmentos} (${tamano(v.bytes)})`],
  ]);
  partes.push(resumen);

  const grave = nota(
    "Esto borra los fragmentos, la raíz y la cabecera de TODOS los proveedores. " +
      "No se puede deshacer y no hay papelera. Lo que no sea del volumen se queda donde está.",
  );
  grave.className = "aviso mal";
  partes.push(grave);

  const etiqueta = document.createElement("label");
  etiqueta.htmlFor = "texto-confirmar";
  etiqueta.textContent = `Para confirmar, escribe: ${v.confirmacion}`;
  const campo = document.createElement("input");
  campo.type = "text";
  campo.id = "texto-confirmar";
  campo.autocomplete = "off";
  campo.spellcheck = false;

  const borrar = document.createElement("button");
  borrar.textContent = "Eliminar el volumen";
  borrar.disabled = true;
  // El botón no se enciende hasta que lo escrito coincide. Así el paso de leer no se puede
  // saltar a base de clics.
  campo.addEventListener("input", () => {
    borrar.disabled = campo.value.trim() !== v.confirmacion;
  });

  const cancelar = document.createElement("button");
  cancelar.className = "secundario";
  cancelar.textContent = "Cancelar";
  cancelar.addEventListener("click", () => {
    caja.replaceChildren();
    caja.hidden = true;
  });

  const fila = document.createElement("div");
  fila.className = "enlinea";
  fila.append(cancelar, borrar);

  const estado = nota("");
  estado.hidden = true;

  borrar.addEventListener("click", async () => {
    borrar.disabled = true;
    cancelar.disabled = true;
    estado.hidden = false;
    estado.className = "nota";
    const parar = contando(estado, "Borrando de cada proveedor…");
    try {
      const dicho = await invoke("borrar_volumen", { confirmacion: campo.value.trim() });
      parar();
      caja.replaceChildren();
      caja.hidden = true;
      // Se vuelve a arrancar: ya no hay volumen, así que la pantalla es otra.
      await arrancar();
      aviso(dicho);
    } catch (err) {
      parar();
      estado.className = "aviso mal";
      estado.textContent = String(err);
      cancelar.disabled = false;
    }
  });

  partes.push(etiqueta, campo, fila, estado);
  caja.replaceChildren(...partes);
  campo.focus();
}
