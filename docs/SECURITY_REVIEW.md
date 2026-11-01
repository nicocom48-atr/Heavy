# Revisión de seguridad, rendimiento y P2P

Fecha: 2026-11-01. Base revisada: `main` en `247b5d5`, incluidos los cambios de protocolo v5 y el release v0.2.8. Alcance: revisión de código, pruebas automatizadas locales y análisis de dependencias. No es una certificación de seguridad ni una prueba de penetración externa.

## Estado P2P

El transporte predeterminado es Iroh/QUIC. Utiliza rutas directas cuando están disponibles y mantiene la posibilidad de migrar desde relay a una ruta directa. El modo **Solo P2P directo** elimina direcciones relay de la invitación recibida y deshabilita su transporte; una invitación que contenga solo relay se rechaza antes de conectar.

Esto maximiza las conexiones directas sin prometer que todos los NAT admitan perforación. Es normal necesitar un relay en algunas redes. La disponibilidad del relay no bloquea la preparación de una invitación local; el modo automático espera un tiempo limitado y puede emitir una invitación con solo direcciones IP. No se probó una matriz de routers ni una transferencia WAN real entre dos redes independientes.

### Política de recepción QUIC

La GUI y `recv` aceptan invitaciones QUIC de forma predeterminada. Recibir un código corto requiere consentimiento explícito: **Compatibilidad TCP (metadatos visibles)** en Ajustes o `--legacy-tcp` en la CLI. La validación se ejecuta antes de crear la carpeta de destino, iniciar descubrimiento LAN o consultar el servidor manual. `--direct-only` y la compatibilidad TCP son opciones incompatibles.

El permiso de compatibilidad se aplica a códigos cortos; una invitación QUIC que falle no se reintenta mediante TCP. Esto conserva el cifrado del handshake de la ruta elegida. No modifica el protocolo v5 ni impide el relay cifrado propio de Iroh.

La continuación de política QUIC pasa 50 pruebas locales y Clippy. Incluye regresiones de GUI y CLI, rechazo sin efectos en red/disco y ausencia de fallback TCP cuando expira una invitación QUIC. La prueba pública de Iroh se mantiene opcional; su resultado anterior se indica más abajo.

## Correcciones ya integradas en main

| Área | Problema encontrado | Comportamiento actual |
| --- | --- | --- |
| Autenticación | Solo se autenticaban claves/salt, dejando metadatos y decisiones sin cubrir | Las pruebas v5 autentican oferta completa, respuesta, reanudación y confirmación final |
| X25519 | Se admitían secretos compartidos no contributivos | Se rechazan claves de bajo orden |
| Destino | Un archivo final podía reemplazarse sin `--force` | Se conserva y también se protege la carrera al publicar el `.part` |
| Nombres Windows | Riesgo de ADS y alias de dispositivo con múltiples extensiones | Se rechazan `:`, caracteres ilegales, puntos/espacios finales y nombres reservados |
| Reanudación | El modo sin hash podía mezclar parciales de otro archivo del mismo tamaño | Solo se reanuda con identidad de contenido BLAKE3; el modo rápido reinicia parciales |
| Archivos grandes | El hash previo podía exceder el timeout del handshake | Se prepara antes de abrir la invitación; el timeout de verificación final es independiente |
| QUIC | Ventanas de 0,5–1 MiB limitaban conexiones con RTT alto | Ventana por stream de 16 MiB y ventanas de conexión/envío de 32 MiB |
| Memoria | La cola copiaba plaintext además de ciphertext | Conserva longitud, checksum y ciphertext; no una copia adicional de plaintext |
| Cancelación | Una tarea de lectura podía sobrevivir a la transferencia | Un guard cancela la tarea al caer su propietario |
| Servidor TCP | Tareas y sesiones sin límite global | Hasta 256 clientes concurrentes y 4096 sesiones, con limpieza y límites de registro |

Las ventanas QUIC mayores no demuestran por sí solas una velocidad WAN concreta. El rendimiento depende de RTT, pérdida, discos, CPU, firewall y ruta seleccionada.

## Comprobaciones adicionales de esta continuación

- Transferencia completa con emisor y receptor en política estricta sin relay.
- Rechazo inmediato de invitaciones que solo contienen relay bajo esa política.
- Consumidores concurrentes compartiendo un solo presupuesto de ancho de banda.
- Desactivar un límite despierta una transferencia que estaba esperando.
- Cancelar el propietario libera los recursos del worker.
- Reintentar una publicación con etiqueta existente reconstruye la misma versión y no omite los assets.
- La cola de GitHub admite múltiples publicaciones pendientes; no cancela la anterior por cada push nuevo. Límite del servicio: 100 pendientes.

Resultado local: 46 pruebas de la suite normal aprobadas y la prueba pública opcional de Iroh aprobada por separado (32 KiB sintéticos). Las pruebas del flujo de release también pasaron. La prueba pública conecta dos endpoints de esta misma máquina; confirma acceso a la infraestructura pública, no una ruta WAN real entre dos redes.

## Dependencias y límites de la revisión

`cargo audit` encontró dos avisos en `quick-xml 0.30.0`: [RUSTSEC-2026-0194](https://rustsec.org/advisories/RUSTSEC-2026-0194.html) y [RUSTSEC-2026-0195](https://rustsec.org/advisories/RUSTSEC-2026-0195.html). Cargo.lock los incluye a través de `zbus_xml` / `zbus-lockstep` / AT-SPI de la accesibilidad Unix. `cargo tree -i quick-xml@0.30.0 --target x86_64-pc-windows-msvc --locked --offline` no encuentra ese paquete en el árbol Windows distribuido. Esto no equivale a que todo el árbol multiplataforma esté libre de avisos.

Queda pendiente actualizar ese backend Unix y revisar avisos de mantenimiento de dependencias transitivas (entre ellos number_prefix, paste, ttf-parser y zvariant). No se desactivó accesibilidad para ocultar los avisos ni se eliminó UPnP/NAT traversal. Los avisos no se agregaron a una lista de exclusión.

La autenticación simétrica del código no es un PAKE; el modo TCP expone metadatos y verificadores del handshake a un observador de esa conexión. QUIC cifra ese intercambio y es el transporte recomendado. Los códigos cortos tienen 60 bits de entropía y deben compartirse como credenciales, no publicarse.

Un peer que conoce la dirección del listener puede causar fallos o demoras de conexión. Los timeouts, límites de recursos y autenticación reducen el riesgo, pero no se demuestra resistencia completa a denegación de servicio. Tampoco se evaluaron adversarios con permisos de escritura local sobre la carpeta de destino, dispositivos FAT/exFAT reales ni transferencias de terabytes.

## Referencias primarias

- [Iroh 1.4.1: conexiones directas, relay y cifrado](https://docs.rs/iroh/1.4.1/iroh/)
- [Iroh: rutas de una conexión](https://docs.rs/iroh/1.4.1/iroh/endpoint/struct.Connection.html#method.paths)
- [GitHub: cola de concurrencia de workflows](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)
