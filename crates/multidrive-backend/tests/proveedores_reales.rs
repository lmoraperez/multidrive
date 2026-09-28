//! Suite de conformidad contra proveedores reales.
//!
//! Se salta sola cuando no hay ninguno configurado, para que `cargo test` siga siendo
//! ejecutable sin credenciales. Configurar uno a medias, en cambio, falla: ver `live`.
//!
//! ```sh
//! MULTIDRIVE_TEST_ONEDRIVE_ACCESS_TOKEN=... cargo test -p multidrive-backend --test proveedores_reales -- --nocapture
//! ```

use multidrive_backend::{conformance, live};

#[tokio::test]
async fn los_proveedores_configurados_cumplen_el_contrato() {
    let proveedores = match live::configurados() {
        Ok(p) => p,
        // Que este binario no lleve la aplicacion dentro no es una configuracion invalida:
        // es una propiedad de **esta compilacion**. La copia que se distribuye si la lleva, y
        // quien enlazo la cuenta con ella tiene una configuracion perfectamente buena. Aqui
        // no hay nada que probar, asi que se dice y se sale, como cuando no hay proveedores.
        Err(multidrive_backend::BackendError::SinAplicacion { proveedor, tipo }) => {
            eprintln!(
                "'{proveedor}' esta enlazado pero esta compilacion no trae aplicacion para \
                 {tipo}; no se probo nada real.\n\n\
                 Para probarlo:  MULTIDRIVE_APP_{tipo}_CLIENT_ID=... \
                 MULTIDRIVE_APP_{tipo}_CLIENT_SECRET=... cargo test"
            );
            return;
        }
        Err(e) => panic!(
            "configuracion de entorno invalida: {e}\n\n{}",
            live::ayuda()
        ),
    };

    if proveedores.is_empty() {
        eprintln!(
            "sin proveedores configurados; no se probo nada real.\n\n{}",
            live::ayuda()
        );
        return;
    }

    let mut con_fallos = Vec::new();
    for proveedor in &proveedores {
        let informe = conformance::ejecutar(proveedor).await;
        eprintln!("\n=== {} ===", informe.proveedor);
        for check in &informe.checks {
            let ms = informe
                .duracion_de(check.nombre)
                .unwrap_or_default()
                .as_millis();
            match &check.fallo {
                None => eprintln!("  ok    {:<32} {ms:>6} ms", check.nombre),
                Some(motivo) => {
                    eprintln!("  FALLA {:<32} {ms:>6} ms", check.nombre);
                    eprintln!("        {motivo}");
                    eprintln!("        importa porque: {}", check.porque);
                }
            }
        }
        if let Some(e) = &informe.limpieza {
            eprintln!("  AVISO no se pudo limpiar '{}': {e}", informe.prefijo);
        }
        if !informe.todo_ok() {
            con_fallos.push(informe.proveedor.clone());
        }
    }

    assert!(
        con_fallos.is_empty(),
        "proveedores que no cumplen el contrato de Backend: {}",
        con_fallos.join(", ")
    );
}
