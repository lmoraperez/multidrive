// Sirve site/ en http://localhost:8080 para probar la version web antes de subirla.
//
//   node packaging/web/servir.mjs
//
// Sin dependencias: Node trae todo lo que hace falta. El puerto es el que esta registrado como
// redireccion en Google y en Azure (`http://localhost:8080/app/`); con otro, entrar en las
// cuentas falla con `redirect_uri_mismatch`.
//
// Solo escucha en 127.0.0.1: es para probar en este equipo, no para servir a nadie.

import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const RAIZ = resolve(fileURLToPath(new URL("../../site", import.meta.url)));
const PUERTO = 8080;

// `application/wasm` es el que importa: sin el, el navegador no compila el WebAssembly
// mientras lo descarga y el pegamento de wasm-bindgen cae a la via lenta con un aviso.
const TIPOS = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".wasm": "application/wasm",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".json": "application/json",
};

createServer(async (req, res) => {
  const ruta = decodeURIComponent(new URL(req.url, "http://x").pathname);
  let fichero = normalize(join(RAIZ, ruta));
  // Nada fuera de site/, por mucho `..` que traiga la peticion.
  if (fichero !== RAIZ && !fichero.startsWith(RAIZ + sep)) {
    res.writeHead(403).end();
    return;
  }
  try {
    if ((await stat(fichero)).isDirectory()) fichero = join(fichero, "index.html");
    const cuerpo = await readFile(fichero);
    res.writeHead(200, {
      "Content-Type": TIPOS[extname(fichero)] ?? "application/octet-stream",
      "Cache-Control": "no-store",
    });
    res.end(cuerpo);
  } catch {
    res.writeHead(404, { "Content-Type": "text/plain; charset=utf-8" }).end("no existe");
  }
}).listen(PUERTO, "127.0.0.1", () => {
  console.log(`Sirviendo ${RAIZ} en http://localhost:${PUERTO}/app/`);
});
