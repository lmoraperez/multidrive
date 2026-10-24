// Interfaz de MultiDrive. Sin dependencias: lo que hay aqui es todo lo que se ejecuta.
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

  // Sin volumen abierto, el panel sobra --y esconderlo es cosa de aqui, no de quien llama.
  // Al borrar el volumen desde el propio panel se volvia aqui, se ensenaba la entrada y el
  // panel se quedaba debajo: media ventana hablando de la salud, el espacio, el montaje y lo
  // pendiente de subir de un volumen que ya no existia.
  ocultarPanel();

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
  // Plegado salvo que falte algo: cuando los proveedores configurados no son los del volumen,
  // esas dos líneas son exactamente la pregunta que hay que resolver, y esconderlas detrás de
  // un clic sería esconder el diagnóstico.
  const faltaAlguno =
    a.hay_volumen && a.del_volumen.some((n) => !a.proveedores.includes(n));
  $("detalles-previos").open = faltaAlguno || recordado("detalles-previos");

  // El cupo es de la cuenta del proveedor, no del volumen: se puede ver con el volumen
  // cerrado, y es justo lo que hace falta para decidir si merece la pena crear uno.
  if (a.proveedores.length) refrescarEspacio($("espacio"));
  else $("espacio").replaceChildren(nota("No hay ningún proveedor configurado."));
  // La maleta solo se ofrece cuando se detecta una al enlazar una cuenta. Aqui se esconde
  // siempre: si no, la de la vez anterior se queda abierta pidiendo una frase que ya no hace
  // falta.
  $("equipaje").hidden = true;

  // La visibilidad se decide **siempre**, no solo cuando hay un problema. Antes se fijaba
  // dentro de esa rama, asi que quien creaba un volumen y volvia a esta pantalla se
  // encontraba con que decia «hace falta la frase de paso» y no habia donde escribirla: el
  // formulario seguia oculto de cuando no habia volumen que abrir.
  $("formulario").hidden = !a.hay_volumen;
  // La de aqui; la del panel la limpia `ocultarPanel()`.
  $("confirmar-borrado").hidden = true;
  $("confirmar-borrado").replaceChildren();
  // Y la de crear, por lo mismo: se enseña unas líneas más abajo cuando procede, pero si no
  // se esconde aquí se queda de la vez anterior. Pasó al quitar el último proveedor: seguía
  // ofreciendo «crear sobre los 1 proveedores configurados: gdrive» sin ningún gdrive.
  $("creacion").hidden = true;

  // El aviso se decide **siempre**, igual que la visibilidad de aquí arriba y por el mismo
  // motivo que está escrito ahí: pintarlo solo cuando hay problema deja el de la vez anterior
  // en pantalla diciendo algo que ya no es verdad. Pasó tal cual: se añadía un proveedor y
  // seguía leyéndose «no hay ningún proveedor configurado» con el proveedor listado debajo.
  aviso("");

  if (a.problema) {
    // Sin volumen no hay nada que abrir: pedir la frase seria pedir algo inutil. Pero si hay
    // proveedores configurados, si hay algo que hacer: crearlo.
    if (!a.hay_volumen && a.proveedores.length > 0) {
      $("cabecera").textContent = "No hay ningún volumen todavía.";
      await ofrecerCreacion(a.proveedores);
    } else {
      aviso(a.problema, !a.hay_volumen);
    }
  } else if (a.hay_volumen) {
    // El volumen vive en unos proveedores concretos, y con uno solo de ellos no se abre. Hay
    // que decirlo **antes** de que alguien escriba la frase de paso, no después de que el
    // intento falle: el error de después no distingue «falta una cuenta» de «me equivoqué».
    const faltan = a.del_volumen.filter((n) => !a.proveedores.includes(n));
    if (faltan.length) {
      // Antes de dar el aviso, mirar si la solucion ya esta guardada en una de las cuentas
      // que si hay: si la maleta esta, lo que toca es pedir la frase, no dar un parte.
      if (await ofrecerMaleta()) return;
      aviso(
        `Falta configurar ${faltan.join(", ")}: este volumen está repartido entre ` +
          `${a.del_volumen.length} cuentas y aquí hay ${a.proveedores.length}. ` +
          "Enlázalas para poder abrirlo.",
        true,
      );
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
  const boton = $("cerrar");
  boton.disabled = true;
  try {
    // Cerrar con algo montado dejaria una unidad sirviendo un volumen que la ventana dice
    // haber cerrado. Se desmonta primero, y desmontar hace la última subida.
    await invoke("desmontar");
    try {
      await invoke("cerrar", { forzar: false });
    } catch (err) {
      // Cerrar se niega si queda algo sin subir. No se cierra a la fuerza por nuestra cuenta:
      // se enseña qué ficheros son y se deja decidir, porque a veces no hay más remedio
      // —estás sin red y tienes que irte— y a veces es un descuido que cuesta los datos.
      await refrescarPendiente();
      if (!(await ofrecerCerrarIgualmente(String(err)))) return;
      await invoke("cerrar", { forzar: true });
    }
    await arrancar();
  } finally {
    boton.disabled = false;
  }
});

/**
 * Enseña lo que se queda sin subir y espera una respuesta. Devuelve si hay que cerrar igual.
 *
 * Se pinta al lado del botón de cerrar y no en otra parte de la página, por lo mismo que las
 * confirmaciones de borrado: una advertencia que aparece lejos de donde se pulsó es una
 * advertencia que nadie lee.
 */
function ofrecerCerrarIgualmente(motivo) {
  return new Promise((resolver) => {
    const caja = $("confirmar-cerrar");
    const aviso = document.createElement("div");
    aviso.className = "aviso mal";
    aviso.style.whiteSpace = "pre-wrap";
    aviso.textContent = motivo;

    const fila = document.createElement("div");
    fila.className = "enlinea";

    const quedarse = document.createElement("button");
    quedarse.textContent = "Dejarlo abierto";
    const igual = document.createElement("button");
    igual.className = "secundario";
    igual.textContent = "Cerrar igualmente";

    const terminar = (respuesta) => {
      caja.replaceChildren();
      caja.hidden = true;
      resolver(respuesta);
    };
    // Lo seguro es quedarse, así que es lo que tiene el foco: un Enter de más no puede ser lo
    // que deje los ficheros sin subir.
    quedarse.addEventListener("click", () => terminar(false));
    igual.addEventListener("click", () => terminar(true));

    fila.append(quedarse, igual);
    caja.replaceChildren(aviso, fila);
    caja.hidden = false;
    quedarse.focus();
  });
}

/**
 * Lo que el usuario dejó abierto la última vez.
 *
 * Solo para comodidades de esta pantalla: si el navegador no deja guardar --y puede no
 * dejar-- se sigue igual, plegado, que es el valor por defecto.
 */
function recordado(clave) {
  try {
    return localStorage.getItem(clave) === "1";
  } catch {
    return false;
  }
}

$("detalles-previos").addEventListener("toggle", () => {
  try {
    localStorage.setItem("detalles-previos", $("detalles-previos").open ? "1" : "0");
  } catch {
    // Que no se pueda recordar no es motivo para que la pantalla deje de funcionar.
  }
});

function nota(texto) {
  const p = document.createElement("p");
  p.className = "nota";
  p.textContent = texto;
  return p;
}

/**
 * Deja el panel fuera de la vista y sin restos del volumen que ya no esta.
 *
 * Se borra lo que solo tenia sentido con ese volumen: el parte de salud, el aviso de donde
 * quedo la configuracion y la confirmacion de borrado a medias. Lo demas se repinta entero
 * al volver a abrir, asi que no hace falta tocarlo.
 */
function ocultarPanel() {
  $("panel").hidden = true;
  $("salud").replaceChildren(nota("Sin revisar todavía."));
  $("maleta-dicha").hidden = true;
  $("confirmar-borrado-panel").hidden = true;
  $("confirmar-borrado-panel").replaceChildren();
  $("confirmar-cerrar").hidden = true;
  $("confirmar-cerrar").replaceChildren();
}

async function mostrarPanel(ya) {
  $("entrada").hidden = true;
  $("panel").hidden = false;
  $("cabecera").textContent = "Volumen abierto.";
  // El panel tiene su propio boton de eliminar, junto a «Cerrar»: quien acaba de crear un
  // volumen tiene que poder deshacerlo sin cerrarlo antes.
  $("confirmar-borrado-panel").hidden = true;
  $("confirmar-borrado-panel").replaceChildren();

  // Al arrancar con el volumen ya abierto no hay resumen a mano: se pide.
  const r = ya ?? (await invoke("resumen"));
  rellenarResumen(r);
  // Al crear, la configuración se guarda sola en las nubes. Se dice: es la diferencia entre
  // cambiar de equipo enlazando una cuenta o enlazándolas todas otra vez.
  // Se reutiliza el mismo parrafo en vez de insertar otro: `after()` dejaba una copia mas
  // debajo del resumen en cada apertura.
  $("maleta-dicha").textContent = r.maleta ?? "";
  $("maleta-dicha").hidden = !r.maleta;
  await refrescarMontaje();
  await refrescarPendiente();
  await refrescarEspacio($("espacio-panel"));
  await refrescarAmpliacion();
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
    const partes = [];

    if (p.a_salvo) {
      partes.push(nota("Nada pendiente: todo lo escrito está en los proveedores."));
    } else {
      // Los ficheros por su nombre y no un total de trozos: la pregunta de quien va a apagar
      // el equipo no es «cuántos trozos faltan», es «qué pierdo si lo apago ahora». Y el total
      // mezcla datos con nodos del árbol, así que ni siquiera sirve de aproximación.
      if (p.rutas.length) {
        const aviso = document.createElement("div");
        aviso.className = "aviso mal";
        aviso.textContent =
          `${p.rutas.length} fichero(s) SOLO EN ESTA MÁQUINA. Si pierdes este equipo, ` +
          "se pierden. Se suben solos mientras el volumen esté montado en escritura.";
        partes.push(aviso);

        const lista = document.createElement("ul");
        lista.className = "lista";
        for (const r of p.rutas) {
          const li = document.createElement("li");
          li.textContent = `${r.ruta} — ${tamano(r.bytes)}`;
          lista.append(li);
        }
        partes.push(lista);
      } else {
        // Puede no haber ningún fichero detrás: lo que falta es el índice. Se dice lo que es.
        partes.push(
          nota(
            `${p.trozos} trozo(s) del índice del volumen sin subir, ${tamano(p.bytes)}. ` +
              "Ningún fichero se queda a medias por esto.",
          ),
        );
      }
      if (p.raiz_sin_publicar) {
        partes.push(
          nota(
            "Y el índice está sin publicar: desde otro equipo todavía no se ve nada de lo " +
              "último escrito, aunque los datos ya hayan subido.",
          ),
        );
      }
    }

    // Que haya otro escribiendo no impide nada: los cambios se ponen uno encima del otro. Pero
    // si los dos tocáis el mismo fichero, uno verá un error, y eso es mejor saberlo antes.
    if (p.otro_escribiendo) {
      partes.unshift(
        nota(
          `${p.otro_escribiendo} también está escribiendo en este volumen. Mientras toquéis ` +
            "cosas distintas no pasa nada; el mismo fichero a la vez, no.",
        ),
      );
    }

    // Si el testigo se perdió, lo primero de todo: a partir de ese momento lo que se escriba
    // aquí puede chocar con lo de otro equipo, y eso no es un detalle del final de una lista.
    if (p.testigo_perdido) {
      const malo = document.createElement("div");
      malo.className = "aviso mal";
      malo.textContent =
        "Se perdió el permiso de escritura: otro equipo pudo empezar a escribir en este " +
        "volumen. Desmonta aquí y vuelve a montar para recuperarlo; si algo no sube, es por esto.";
      partes.unshift(malo);
    }

    // Lo último que hizo el sincronizador de fondo. Se guardaba y no lo leía nadie: un
    // sincronizador que falla en silencio es justo el que no hay que tener.
    if (p.ultimo_sync) partes.push(nota(`Última pasada: ${p.ultimo_sync}.`));

    caja.replaceChildren(...partes);
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
      nota(`Montado en ${m.punto}.`),
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

  const error = nota("");
  error.hidden = true;

  boton.addEventListener("click", async () => {
    boton.disabled = true;
    error.hidden = true;
    try {
      // Siempre en lectura y escritura: una unidad que no deja escribir no es una unidad,
      // y tenerlo como casilla obligaba a decidir algo que nadie quiere decidir.
      await invoke("montar", { punto: campo.value, escritura: true });
      await refrescarMontaje();
      await refrescarPendiente();
    } catch (err) {
      error.textContent = String(err);
      error.hidden = false;
    } finally {
      boton.disabled = false;
    }
  });

  caja.replaceChildren(fila, error);
}

// ---------------------------------------------------------------------------------------
// Crear un volumen
// ---------------------------------------------------------------------------------------

/**
 * El fichero del logotipo de un tipo de proveedor, o `null` si no hay.
 *
 * Se devuelve `null` en vez de un icono genérico a propósito: un botón sin logotipo se lee
 * igual, y uno con el logotipo equivocado hace dudar de si se está conectando lo que se cree.
 */
function iconoDe(clave) {
  const cuales = { GDRIVE: "gdrive", ONEDRIVE: "onedrive" };
  const cual = cuales[clave];
  return cual ? `iconos/${cual}.png` : null;
}

async function ofrecerCreacion(proveedores) {
  $("creacion").hidden = false;
  $("sobre-proveedores").textContent =
    `Se creará sobre los ${proveedores.length} proveedores configurados: ` +
    `${proveedores.join(", ")}. Para usar otros, cámbialos en la configuración y reinicia.`;

  // Preguntar el cupo es una peticion por nube, asi que se dice que se esta haciendo.
  $("precio").replaceChildren(nota("Consultando cuánto espacio hay en cada proveedor…"));
  const opciones = await invoke("topologias", { cuantos: proveedores.length });
  const lista = $("topologias");
  lista.replaceChildren();

  for (const o of opciones) {
    const tarjeta = document.createElement("button");
    tarjeta.type = "button";
    tarjeta.className = "topologia";
    tarjeta.setAttribute("role", "radio");
    tarjeta.dataset.spec = o.spec;
    tarjeta.dataset.tolera = String(o.tolera);
    tarjeta.dataset.sobrecoste = o.sobrecoste.toFixed(2);
    tarjeta.dataset.aprovechado = String(Math.round(o.aprovechado * 100));
    tarjeta.dataset.sinRed = o.sin_red ? "1" : "";
    if (o.libre !== null) tarjeta.dataset.libre = String(o.libre);
    if (o.sin_medir.length) tarjeta.dataset.sinMedir = o.sin_medir.join(", ");
    if (o.suma) tarjeta.dataset.suma = "1";

    const nombre = document.createElement("b");
    nombre.textContent = o.spec;

    // Las dos cifras con las que se elige, una debajo de otra y siempre en el mismo orden,
    // que es lo que permite comparar en vertical sin leer frases.
    const aguanta = document.createElement("span");
    aguanta.className = o.sin_red ? "mal" : "";
    aguanta.textContent = o.sin_red
      ? "No aguanta perder ninguna cuenta."
      : `Aguanta perder ${o.tolera} cuenta(s).`;

    const sitio = document.createElement("span");
    sitio.className = "cifra";
    sitio.textContent =
      o.libre !== null ? `Espacio disponible: ${tamano(o.libre)}.` : "Espacio sin medir.";

    tarjeta.append(nombre, aguanta, sitio);
    tarjeta.addEventListener("click", () => elegirTopologia(o.spec));
    lista.append(tarjeta);
  }

  // La primera de la lista es la que más aguanta. Que el valor por defecto sea el prudente
  // importa: quien no sepa elegir se queda con el que menos sorpresas da.
  if (opciones.length) elegirTopologia(opciones[0].spec);
}

/** Marca una opción como elegida y repinta el detalle de debajo. */
function elegirTopologia(spec) {
  $("topologia").value = spec;
  for (const t of $("topologias").children) {
    const suya = t.dataset.spec === spec;
    t.classList.toggle("elegida", suya);
    t.setAttribute("aria-checked", suya ? "true" : "false");
  }
  mostrarPrecio();
}

function mostrarPrecio() {
  const op = [...$("topologias").children].find((t) => t.dataset.spec === $("topologia").value);
  if (!op) return;
  const caja = $("precio");
  caja.replaceChildren();

  // Lo primero, la cifra con la que de verdad se decide. «Aprovechas el 50 %» no dice si te
  // quedan tres gigas o trescientos, y con proveedores desiguales el número sorprende:
  // manda el más apretado, así que 15 GiB y 3,4 GiB dan 3,4 en espejo y 6,8 en unión.
  if (op.dataset.libre) {
    const cuanto = document.createElement("p");
    cuanto.className = "cuanto";
    cuanto.textContent = `Te quedarían ${tamano(Number(op.dataset.libre))} libres.`;
    caja.append(cuanto);
  }

  const detalle = document.createElement("p");
  if (op.dataset.sinRed) {
    // Sin redundancia, el aviso no es un matiz del precio: es lo único que hay que saber
    // antes de elegirlo. Va en rojo y con todas las letras.
    detalle.className = "aviso mal";
    detalle.textContent = op.dataset.suma
      ? "NO aguanta ninguna caída: si un proveedor falla o te cierra la cuenta, pierdes el " +
        "volumen ENTERO. A cambio suma el espacio de todos, aunque sean de tamaños muy " +
        "distintos, y leer cuesta una petición en vez de una por proveedor."
      : "NO aguanta ninguna caída: si un proveedor falla o te cierra la cuenta, pierdes el " +
        "volumen ENTERO, no una parte. A cambio aprovechas el 100 % del espacio y repartes " +
        "la lectura y la escritura entre todos.";
  } else {
    detalle.className = "nota";
    detalle.textContent =
      `Aguanta perder ${op.dataset.tolera} proveedor(es). ` +
      `Aprovechas el ${op.dataset.aprovechado}% del espacio: guardar 1 TB ocupa ` +
      `${op.dataset.sobrecoste} TB repartidos.`;
  }
  caja.append(detalle);

  if (op.dataset.libre) {
    // La letra pequeña no es un descargo: es lo que evita que alguien planifique con una
    // cifra que no significa lo que parece. Y no significa lo mismo en las dos: al repartir
    // cada trozo entre todos manda el más pequeño, y al poner cada trozo en uno se suman.
    caja.append(
      nota(
        op.dataset.suma
          ? "Cada trozo va entero a un proveedor, elegido en proporción a lo que cada uno " +
              "aporta, así que el espacio se suma aunque las cuentas sean muy desiguales. " +
              "Sale de lo que declara cada nube, que va por detrás de lo que acabas de escribir."
          : "Manda el proveedor con menos sitio: los trozos se reparten a partes iguales, " +
              "así que cuando el más pequeño se llena no cabe nada más aunque a los otros " +
              "les sobre. Y sale de lo que declara cada nube, que va por detrás de lo que " +
              "acabas de escribir.",
      ),
    );
  }
  if (op.dataset.sinMedir) {
    // En las que reparten, dejar fuera a uno da un número optimista: podría ser el más
    // apretado. En la que suma es al revés — falta su aporte, así que se queda corto.
    const aviso = nota(
      `${op.dataset.suma ? "Se queda corta" : "Es un techo, no una medida"}: ` +
        `${op.dataset.sinMedir} no declara${
          op.dataset.sinMedir.includes(",") ? "n" : ""
        } su cupo, así que la cifra se ha calculado sin contarlo.`,
    );
    aviso.className = "aviso";
    caja.append(aviso);
  }
}

/**
 * Compara las dos frases mientras se escriben y gobierna el botón de crear.
 *
 * El detalle que hace esto usable es **cuándo se calla**: mientras lo repetido encaja con el
 * principio de lo primero, no se dice nada. Quejarse en cada letra de una frase a medio
 * teclear convierte el aviso en ruido, y un aviso que siempre está puesto deja de leerse
 * justo el día que significa algo.
 *
 * El botón arranca apagado y sólo se enciende cuando coinciden de verdad. Sin recuperación
 * posible, una errata aquí es un volumen perdido antes de empezar.
 */
function revisarFrases() {
  const a = $("frase-nueva").value;
  const b = $("frase-otra").value;
  const dicho = $("coinciden");
  const boton = $("boton-crear");

  if (!a || !b) {
    dicho.hidden = true;
    boton.disabled = true;
    return;
  }
  if (a === b) {
    dicho.hidden = false;
    dicho.textContent = "Las dos coinciden.";
    dicho.style.color = "var(--accent)";
    boton.disabled = false;
    return;
  }
  boton.disabled = true;
  if (a.startsWith(b)) {
    // Va bien, sólo le falta. Callado.
    dicho.hidden = true;
    return;
  }
  dicho.hidden = false;
  dicho.textContent = "No coinciden.";
  dicho.style.color = "var(--mal)";
}

for (const id of ["frase-nueva", "frase-otra"]) {
  $(id).addEventListener("input", revisarFrases);
}

$("form-crear").addEventListener("submit", async (e) => {
  e.preventDefault();
  const a = $("frase-nueva");
  const b = $("frase-otra");
  // El botón ya está apagado si no coinciden, pero la comprobación se queda: un formulario
  // se puede enviar con Enter desde un campo, y lo que protege de verdad es lo que se mira
  // aquí, no el estado de un botón.
  if (a.value !== b.value) {
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
    // Vaciar los campos sin volver a mirarlos dejaria el boton encendido sobre dos campos
    // vacios, listo para crear un volumen con frase vacia si la seccion se vuelve a ensenar.
    revisarFrases();
    $("creacion").hidden = true;
    await mostrarPanel(r);
  } catch (err) {
    aviso(String(err), true);
  } finally {
    $("boton-crear").disabled = false;
  }
});

// El pie con la version. Va aparte del arranque a proposito: si lo de arriba falla, esto es
// justo lo que hace falta saber para reportarlo, asi que tiene que salir igual.
invoke("version")
  .then((v) => {
    $("pie").textContent = `MultiDrive ${v} · alfa`;
  })
  .catch(() => {
    // Ni siquiera esto se puede dar por hecho, y es preferible no decir nada a decir una
    // version inventada.
    $("pie").textContent = "MultiDrive · alfa";
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
 * un cero seria afirmar que MultiDrive no ocupa nada ahi.
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


/**
 * Pregunta antes de olvidar una cuenta, en el propio bloque del proveedor.
 *
 * Se dice lo que hace y lo que **no** hace: no borra nada de la nube. La gente que ve una X
 * al lado de un proveedor con 13 GB dentro tiene todo el derecho a temerse lo peor.
 */
function confirmarOlvido(nombre, bloque) {
  if (bloque.querySelector(".olvido")) return;

  const caja = document.createElement("div");
  caja.className = "olvido";

  const texto = nota(
    `¿Olvidar «${nombre}»? Se borra de esta configuración la forma de llegar a esa cuenta. ` +
      "No se borra nada de la nube, y si el proveedor forma parte de un volumen, ese volumen " +
      "dejará de poder abrirse hasta que lo vuelvas a enlazar.",
  );

  const si = document.createElement("button");
  si.className = "peligro";
  si.textContent = "Olvidar";

  const no = document.createElement("button");
  no.className = "secundario";
  no.textContent = "Cancelar";
  no.addEventListener("click", () => caja.remove());

  si.addEventListener("click", async () => {
    si.disabled = true;
    no.disabled = true;
    try {
      const dicho = await invoke("olvidar_proveedor", { nombre });
      // Se repinta todo: la lista de proveedores, las barras y lo que dependa de ellas.
      await arrancar();
      await prepararAlta();
      const caja2 = $("espacio-panel").hidden === false ? $("espacio-panel") : $("espacio");
      caja2.prepend(nota(dicho));
    } catch (err) {
      const mal = nota(String(err));
      mal.className = "aviso mal";
      caja.replaceChildren(mal, no);
      no.disabled = false;
      no.textContent = "Cerrar";
    }
  });

  const fila = document.createElement("div");
  fila.className = "enlinea";
  fila.append(si, no);
  caja.append(texto, fila);
  bloque.append(caja);
}
function pintarProveedor(p) {
  const bloque = document.createElement("div");
  bloque.className = "proveedor";

  const cabeza = document.createElement("div");
  cabeza.className = "cabeza";
  const nombre = document.createElement("b");
  nombre.textContent = p.proveedor;
  const cifra = document.createElement("span");
  cifra.className = "cifra";

  // Olvidar la cuenta. No saca nada de la nube ni del volumen: borra de aquí la forma de
  // llegar. Por eso pregunta antes, y por eso el mando del otro lado se niega si el
  // proveedor es del volumen que está abierto.
  const quitar = document.createElement("button");
  quitar.className = "quitar";
  quitar.type = "button";
  quitar.textContent = "✕";
  quitar.title = `Olvidar ${p.proveedor}`;
  quitar.setAttribute("aria-label", `Olvidar ${p.proveedor}`);
  quitar.addEventListener("click", () => confirmarOlvido(p.proveedor, bloque));

  cabeza.append(nombre, cifra, quitar);
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

  // El tramo de MultiDrive nunca se pinta mayor que lo que el proveedor dice que hay
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
      //
      // Con el logotipo dentro, como en la aplicación de Android. No es adorno: entre varios
      // botones azules iguales, el logotipo es lo que se reconoce antes de leer, y éste es un
      // sitio donde equivocarse de proveedor hace perder un rato.
      boton.className = "con-icono";
      const icono = iconoDe(t.clave);
      if (icono) {
        const img = document.createElement("img");
        img.src = icono;
        img.alt = "";
        boton.append(img);
      }
      boton.append(document.createTextNode(`Conectar con ${t.etiqueta}`));
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
/**
 * Quien dice «ahora no» no quiere que se lo vuelvan a preguntar en cada repintado. Se olvida
 * al enlazar otra cuenta, que es un motivo nuevo para ofrecerla.
 */
let maletaRechazada = false;

async function guardado(g) {
  let texto = `Guardado como «${g.nombre}». Ya aparece en la lista de arriba.`;
  if (g.hay_volumen) {
    // Configurar no es meterlo en el volumen, y son cosas de coste muy distinto. Decir aquí
    // que ya está dentro sería la mentira más cara de esta pantalla.
    texto +=
      "\n\nOJO: ya hay un volumen, y esto NO lo mete dentro: para eso hay que mover " +
      "fragmentos. Abre el volumen y usa «Ampliar el volumen», que dice antes de empezar " +
      "qué se gana y cuánto hay que mover. Si lo que hace falta es reescribir el volumen " +
      "entero, lo dirá y habrá que hacerlo desde la línea de órdenes.";
  }
  await arrancar();
  // Después de repintar, porque repintar borra el resultado anterior.
  await prepararAlta();
  resultado(texto);

  // Enlazar una cuenta es motivo nuevo para ofrecerla, aunque antes se dijera «ahora no».
  maletaRechazada = false;
  // Si esa cuenta viene de otro equipo, ya trae dentro la configuración del resto. Se ofrece
  // aquí mismo: esperar a que alguien encuentre un botón que no sabe que existe es la forma
  // de que enlace las cuentas una a una habiéndolas guardado precisamente para no hacerlo.
  await ofrecerMaleta();
}

/**
 * Pregunta si las cuentas ya guardan una maleta y, si la hay, abre la sección pidiendo la
 * frase de paso.
 *
 * Cuesta una consulta por proveedor, asi que se pregunta solo en los dos momentos en que la
 * respuesta importa: al enlazar una cuenta, y al arrancar con un volumen al que le faltan
 * proveedores. Devuelve si se ofrecio.
 */
async function ofrecerMaleta() {
  if (maletaRechazada) return false;
  let hay = false;
  try {
    hay = await invoke("hay_maleta");
  } catch {
    // No poder mirar no es motivo para molestar: la pantalla normal sigue funcionando.
    return false;
  }
  if (!hay) return false;

  $("equipaje").hidden = false;
  // Con la maleta delante, el formulario de abrir sobra: pide la misma frase para lo mismo,
  // y además no serviría todavía, porque faltan proveedores que están dentro de la maleta.
  // Dos campos de frase de paso en la misma pantalla es una pantalla que no se entiende.
  $("formulario").hidden = true;
  aviso("");
  $("resultado-maleta").hidden = true;
  $("frase-maleta").focus();
  return true;
}

// ---------------------------------------------------------------------------------------
// Retirar el volumen
// ---------------------------------------------------------------------------------------

// Hay dos botones de eliminar --uno junto a «Abrir volumen» y otro en el panel-- porque se
// puede llegar con el volumen cerrado o abierto. Cada uno pinta la confirmacion en su propia
// caja, que es la que tiene al lado: una confirmacion que aparece en otra parte de la pagina
// es una confirmacion que nadie lee.
for (const boton of document.querySelectorAll(".pedir-borrar")) {
  boton.addEventListener("click", async () => {
    const caja = $(boton.dataset.destino);
    boton.disabled = true;
    caja.hidden = false;
    const parar = contando(
      caja.appendChild(nota("")),
      "Mirando qué hay que borrar; enumera cada proveedor y puede tardar…",
    );
    try {
      const v = await invoke("volumen_a_borrar");
      parar();
      pedirConfirmacion(v, caja);
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
}

/**
 * Enseña qué se va a borrar y pide escribirlo para confirmar.
 *
 * Se pide **escribir los nombres de los proveedores** y no pulsar «sí»: obliga a mirar la
 * lista. Un botón que solo hay que pulsar se pulsa sin leer, y esto no tiene vuelta atrás.
 */
function pedirConfirmacion(v, caja) {
  const partes = [];

  const resumen = document.createElement("dl");
  definiciones(resumen, [
    ["topologia", v.layout],
    ["proveedores", v.proveedores.join(", ")],
    ["fragmentos", `${v.fragmentos} (${tamano(v.bytes)})`],
  ]);
  partes.push(resumen);

  // La configuración sellada se nombra aparte y no dentro de la lista de arriba: es lo único
  // que seguiría sirviendo para algo después de borrar, así que quien confirma tiene que leer
  // que también se va.
  if (v.con_maleta.length) {
    partes.push(
      nota(
        `Y la configuración sellada que hay en ${v.con_maleta.join(", ")}. ` +
          "Sin volumen no abre nada, y dejarla sería no devolver la cuenta a como estaba.",
      ),
    );
  }

  const grave = nota(
    "Esto borra los fragmentos, la raíz y la cabecera de TODOS los proveedores. " +
      "No se puede deshacer y no hay papelera. Lo que no sea del volumen se queda donde está.",
  );
  grave.className = "aviso mal";
  partes.push(grave);

  // Al borrar se desenlazan tambien las cuentas, igual que en Android. Para las de nube eso
  // son dos clics de volver a enlazar; para las escritas a mano son credenciales tecleadas
  // que no estan en ningun otro sitio, asi que se nombran una por una. Avisar «se perdera la
  // configuracion» sin decir de cuales es no avisar de nada.
  const desenlace = nota(
    v.a_mano.length
      ? "Además se desenlazarán todas las cuentas de este equipo. " +
          `Ojo con ${v.a_mano.join(", ")}: son de configuración escrita a mano, así que ` +
          "habrá que volver a teclear sus credenciales. Tenlas a mano antes de seguir."
      : "Además se desenlazarán todas las cuentas de este equipo, y queda como recién " +
          "instalado. Son cuentas de nube: se vuelven a enlazar desde el navegador.",
  );
  if (v.a_mano.length) desenlace.className = "aviso";
  partes.push(desenlace);

  const etiqueta = document.createElement("label");
  etiqueta.htmlFor = "texto-confirmar";
  // Se dice la posición, **no el nombre**: si se enseñara el nombre no habría que mirar la
  // lista, que es lo único que esto pretende. Antes se pedía la lista entera, que obligaba a
  // leerla igual pero crecía con el número de cuentas hasta volverse algo que se copia y se
  // pega sin leer.
  etiqueta.textContent =
    `Para confirmar, escribe el nombre del proveedor n.º ${v.posicion} ` +
    "de la lista de arriba";
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

// ---------------------------------------------------------------------------------------
// Ampliar el volumen
// ---------------------------------------------------------------------------------------

/**
 * Qué se gana metiendo en el volumen los proveedores que están configurados y sueltos.
 *
 * Se consulta al abrir el panel y después de cada cambio, porque la respuesta depende de lo
 * que haya configurado en ese momento.
 */
async function refrescarAmpliacion() {
  const caja = $("ampliacion");
  try {
    pintarAmpliacion(await invoke("ampliacion"));
  } catch (err) {
    caja.replaceChildren(nota(String(err)));
  }
}

function pintarAmpliacion(a) {
  const caja = $("ampliacion");
  if (a.problema) {
    return caja.replaceChildren(nota(a.problema));
  }

  // Lo que se gana, dicho sin adornos. Una copia más no es más espacio, y llamarlo «ampliar»
  // a secas haría esperar sitio donde solo hay margen.
  const gana =
    a.gana === "espacio"
      ? a.suma === null
        ? "más espacio, aunque no dicen cuánto"
        : `${tamano(a.suma)} más de espacio`
      : "una caída más de margen, sin añadir espacio";

  const partes = [
    nota(`Añadir ${a.nuevos.join(", ")}: ${a.antes} → ${a.despues}, ${gana}.`),
  ];
  if (a.gana === "espacio") {
    partes.push(
      nota(
        `Hay que mover ~${Math.round(a.mueve * 100)} % del volumen: solo la parte que le ` +
          "toca al que llega. El volumen se sigue leyendo mientras tanto, y si se corta no " +
          "se pierde nada: lo copiado no se repite.",
      ),
    );
  }

  const boton = document.createElement("button");
  boton.textContent = `Añadir al volumen (${a.nuevos.length})`;
  boton.addEventListener("click", () => confirmarAmpliacion(a));
  partes.push(boton);

  caja.replaceChildren(...partes);
}

/**
 * Pide confirmar antes de empezar.
 *
 * No por peligro —si se corta, el volumen se queda como estaba— sino por tiempo: esto mueve
 * datos entre nubes y puede tardar. Empezarlo sin querer sería dejar la ventana ocupada
 * durante horas.
 */
function confirmarAmpliacion(a) {
  const caja = $("ampliacion");
  const aviso = nota(
    "Desmonta la unidad antes, si la tienes montada. Mientras esto corre no cierres la " +
      "ventana.",
  );
  aviso.className = "aviso";

  const si = document.createElement("button");
  si.textContent = "Empezar";
  si.addEventListener("click", () => ampliarAhora(a));

  const no = document.createElement("button");
  no.className = "secundario";
  no.textContent = "Ahora no";
  no.addEventListener("click", () => pintarAmpliacion(a));

  const fila = document.createElement("div");
  fila.className = "enlinea";
  fila.append(si, no);

  caja.replaceChildren(aviso, fila);
}

async function ampliarAhora(a) {
  const caja = $("ampliacion");
  const linea = nota("");
  caja.replaceChildren(linea);
  const parar = contando(
    linea,
    a.gana === "espacio"
      ? "Moviendo lo que le toca al proveedor nuevo…"
      : "Escribiendo la redundancia que falta…",
  );

  try {
    const r = await invoke("ampliar");
    parar();
    rellenarResumen(r);
    caja.replaceChildren(nota(`Hecho: el volumen es ${r.layout}.`));
    // Las barras cambian: hay un proveedor más y lo que ocupa cada uno ya no es lo que era.
    await refrescarEspacio($("espacio-panel"));
    $("salud").replaceChildren(nota("Sin revisar desde la ampliación."));
    await refrescarAmpliacion();
  } catch (err) {
    parar();
    const mal = nota(String(err));
    mal.className = "aviso mal";
    const otra = document.createElement("button");
    otra.className = "secundario";
    otra.textContent = "Volver";
    otra.addEventListener("click", refrescarAmpliacion);
    caja.replaceChildren(mal, otra);
  }
}

// ---------------------------------------------------------------------------------------
// La maleta: la configuración, cifrada, en las nubes del propio usuario
// ---------------------------------------------------------------------------------------

$("cerrar-equipaje").addEventListener("click", async () => {
  maletaRechazada = true;
  $("equipaje").hidden = true;
  // La frase no se queda escrita en un campo de una ventana que sigue abierta.
  $("frase-maleta").value = "";
  $("resultado-maleta").hidden = true;
  // Se repinta: la oferta escondio el formulario de abrir, y quien dice «ahora no» tiene que
  // recuperar la pantalla de siempre, con su aviso de lo que falta.
  await arrancar();
});

function resultadoMaleta(texto, mal = false) {
  const caja = $("resultado-maleta");
  caja.className = mal ? "aviso mal" : "nota";
  caja.textContent = texto;
  caja.hidden = false;
}

/**
 * Trae la maleta con el boton bloqueado mientras tanto.
 *
 * Tarda: derivar la clave con Argon2id es lento **a propósito**, y además hay que hablar con
 * cada nube. Sin bloquear, dos clics seguidos lanzan dos veces lo mismo.
 */
async function conLaMaleta(texto, trabajo) {
  const boton = $("traer-maleta");
  boton.disabled = true;
  const linea = $("resultado-maleta");
  linea.className = "nota";
  linea.hidden = false;
  const parar = contando(linea, texto);
  try {
    await trabajo();
  } catch (err) {
    parar();
    resultadoMaleta(String(err), true);
  } finally {
    parar();
    boton.disabled = false;
  }
}

$("traer-maleta").addEventListener("click", () =>
  conLaMaleta("Buscando la maleta y abriéndola…", async () => {
    const m = await invoke("recuperar_maleta", { frase: $("frase-maleta").value });
    let texto = `Encontrada en ${m.donde[0]}, ${m.hecha}. Lleva ${m.claves} clave(s).`;
    texto += m.nuevas
      ? `\n\nSe han añadido ${m.nuevas} a este equipo.`
      : "\n\nNo había nada que añadir: este equipo ya tenía lo que lleva.";
    if (m.distintas) {
      // Pisar una credencial que funciona con otra de una maleta vieja es la forma más
      // rápida de quedarse sin acceso a una cuenta, así que no se toca y se dice.
      texto +=
        `\n\nHay ${m.distintas} clave(s) que aquí valen otra cosa y NO se han tocado. ` +
        "Si las de la maleta son las buenas, desde el terminal:\n" +
        "  multidrive config recuperar --sobrescribir";
    }
    // La misma frase abre la maleta y el volumen: es la misma. Se intenta abrirlo sin
    // preguntar otra vez, que es a lo que se venía; si no hay volumen todavía, o si no abre,
    // se dice y se queda en esta pantalla.
    const frase = $("frase-maleta").value;
    $("frase-maleta").value = "";
    resultadoMaleta(texto);
    await arrancar();
    await prepararAlta();

    if (!$("panel").hidden) return;
    try {
      const r = await invoke("abrir", { frase });
      await mostrarPanel(r);
    } catch (err) {
      resultadoMaleta(`${texto}\n\nEl volumen no se pudo abrir todavía: ${err}`);
    }
  }),
);
