# Revisión de seguridad, rendimiento y P2P

Fecha: 2026-11-02. Base revisada: Protocolo v6, transporte multi-stream QUIC con Iroh 1.4.1 / noq-proto. Alcance: mitigación de RUSTSEC-2026-0185 / saturación de stream buffer gaps, desacoplamiento de pipeline de I/O, pruebas automatizadas locales y análisis de dependencias.

## Estado P2P y Transporte QUIC (Protocolo v6)

El transporte predeterminado es Iroh/QUIC con ALPN `heavydrop/6`. Utiliza rutas directas cuando están disponibles y mantiene la posibilidad de migrar desde relay a una ruta directa. El modo **Solo P2P directo** elimina direcciones relay de la invitación recibida y deshabilita su transporte; una invitación que contenga solo relay se rechaza antes de conectar.

### Mitigación de errores de ensamblado de streams QUIC (`too many gaps in stream buffer`)

La crate `iroh 1.4.1` utiliza `noq-proto v1.4.0` (fork de Quinn). Con enlaces de alta velocidad o redes con jitter y pérdida, los fragmentos fuera de orden acumulaban huecos en el búfer de reensamblado de un stream único, disparando un `INTERNAL_ERROR` de QUIC.

Para resolver este problema de raíz:
1. **Pipeline Desacoplado:** La tarea de red lee directamente desde el `RecvStream` de QUIC a máxima velocidad sin realizar operaciones criptográficas ni I/O bloqueante de disco. Envía los bloques recibidos a través de un canal acotado `tokio::sync::mpsc` (capacidad de 8 bloques = máx 32 MiB en tránsito). Una tarea trabajadora separada realiza el descifrado ChaCha20-Poly1305, la verificación BLAKE3 y la escritura en disco mediante `tokio::task::spawn_blocking`.
2. **Ventanas Conservadoras Anti-Gaps:**
   - `DEFAULT_STREAM_RECEIVE_WINDOW`: 8 MiB (evita acumulación masiva de huecos en el reensamblador por stream).
   - `DEFAULT_CONN_RECEIVE_WINDOW`: 32 MiB (flujo global de la conexión).
   - `DEFAULT_SEND_WINDOW`: 32 MiB (límite de envío del emisor).
3. **Multi-Stream Paralelo:** La carga de reensamblado se distribuye entre **4 streams QUIC paralelos**. Cada stream maneja un rango disjunto de bloques, dividiendo los huecos de reensamblado entre 4 estructuras independientes de `noq-proto` y previniendo el bloqueo de cabeza de línea.
4. **Reconexión y Reanudación Automática:** Ante cualquier desconexión transitoria de red, el receptor ejecuta hasta 10 intentos con backoff exponencial. El emisor mantiene su servidor abierto (`SenderServer`) y reanuda la transferencia desde el último bloque verificado en el archivo `.part.meta`.

## Correcciones integradas en el protocolo

| Área | Problema encontrado | Comportamiento en Protocolo v6 |
| --- | --- | --- |
| QUIC Buffer Gaps | Caídas con `too many gaps in stream buffer` en enlaces con fluctuación | Ventana de stream reducida a 8 MiB + 4 streams paralelos balanceados |
| Bloqueo de Red/Disco | Tarea de red bloqueada por descifrado y escritura sincrónica en disco | Pipeline desacoplado: red alimenta cola mpsc, descifrado y disco en `spawn_blocking` |
| Resiliencia ante Caídas | Caída definitiva de conexión ante un corte transitorio | Reconexión automática con backoff exponencial (10 intentos) y persistencia del servidor |
| Autenticación | Riesgo de reutilización prematura de código | Sesión consumida solo tras la confirmación mutua autenticada final (`COMPLETION_ACK`) |
| X25519 | Secretos compartidos no contributivos | Rechazo estricto de claves de bajo orden |
| Nombres Windows | Inyección de rutas, ADS (`:stream`), caracteres prohibidos y alias de dispositivo | Validación completa: rechazo de `:`, `?`, `*`, puntos/espacios finales y nombres reservados DOS |
| Reanudación | Posible corrupción de parciales | Verificación estricta de hash de bloque BLAKE3 e intervalos continuos en `.part.meta` |
| Observabilidad | Errores crípticos de Quinn para el usuario | Mensajes amigables en español, `--verbose` en CLI y desplegable "Detalles técnicos" en GUI |

## Verificación de Integridad y Pruebas Locales

La suite de pruebas automatizadas incluye:
- `test_quic_mid_transfer_interruption_and_resume`: Transfiere archivo grande sobre QUIC, interrumpe abruptamente la conexión a mitad de la transferencia, reconecta el receptor al mismo servidor, reanuda desde los bloques verificados y comprueba que el hash BLAKE3 final coincide al 100%.
- Pruebas de compatibilidad TCP (código corto) y fallback a relay local (`iroh-relay`).
- Pruebas de saneamiento estricto contra path traversal y nombres reservados de Windows.
- Pruebas de consumo concurrente y desconexión cooperativa.

Resultado: 51 pruebas aprobadas con cero fallos (`cargo test --all-targets`) y cero advertencias de Clippy (`cargo clippy`).
