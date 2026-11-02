# HeavyDrop

Transferencia de archivos grandes entre equipos, con interfaz nativa para Windows y CLI en Rust. El transporte predeterminado usa QUIC mediante Iroh, busca una ruta P2P directa y mantiene un relay cifrado como respaldo cuando el NAT o el firewall impiden esa ruta.

Las distribuciones oficiales son para **Windows x64**. Descargas: [Releases](https://github.com/nicocom48-atr/Heavy/releases).

## Conectar dos equipos

1. Selecciona o arrastra el archivo en **Enviar archivo** y pulsa **Crear transferencia**.
2. Copia la **invitación completa `heavy1_…`** y compártela con el receptor. Contiene la identidad y las rutas del emisor; no necesita un servidor manual de códigos cortos.
3. El receptor pega la invitación en **Código del emisor**, elige una carpeta y pulsa **Conectar**.
4. La invitación tiene cinco minutos de validez. El servidor temporal del emisor permanece a la escucha para admitir reconexiones automáticas si ocurre una fluctuación o corte temporal de red.

El diseño **Noche / Cian** integra Enviar, Recibir, Historial, Velocidad y Ajustes en una pantalla adaptable. La ventana inicial mide 1280 × 1040 y conserva desplazamiento en pantallas pequeñas. La GUI se abre sin consola adicional, también en desarrollo.

## ¿Seguimos siendo P2P?

Sí. El modo automático usa direcciones IP directas y perforación de NAT cuando es posible. Iroh puede iniciar la conexión por relay y pasar a una ruta directa; el monitor muestra la ruta seleccionada mientras se transfiere. El relay reenvía tráfico cifrado: no almacena el archivo como un servicio de nube.

- **Automático, recomendado:** permite conexión directa LAN/Internet y relay como respaldo. No exige que un relay esté disponible para habilitar una transferencia local. Si no estuvo disponible durante la preparación, la invitación puede contener únicamente rutas directas.
- **Solo P2P directo (sin relay):** desactiva relays tanto al enviar como al recibir. Puede fallar entre redes con NAT/CGNAT o firewalls restrictivos. No es una forma de atravesar cualquier NAT sin infraestructura auxiliar.
- **Códigos cortos (TCP compatible):** modo explícito de compatibilidad. Usa descubrimiento LAN y, opcionalmente, el servidor manual de señalización/relay. Completar ese servidor no cambia silenciosamente el transporte automático a TCP.

El receptor también exige habilitar esa compatibilidad: en la GUI se activa en Ajustes; en la CLI se utiliza `recv --legacy-tcp`. Sin esa elección, los códigos cortos se rechazan antes de crear carpetas o contactar servidores. Una invitación QUIC fallida nunca se convierte automáticamente en una conexión TCP.

Las invitaciones usan ALPN `heavydrop/6` y protocolo v6. **Actualiza ambos equipos**: no son compatibles con clientes de versiones anteriores.

## Arquitectura de Alta Velocidad (Protocolo v6)

Para transferencias masivas de 10 GB a 1 TB+ a través de Internet con fluctuaciones, latencia y pérdida de paquetes, la versión 6 introduce:

1. **Lectura Desacoplada (Decoupled Pipeline):** En el receptor, tareas asíncronas dedicadas se encargan exclusivamente de drenar los streams QUIC a máxima velocidad hacia un canal acotado (`tokio::sync::mpsc`). El descifrado ChaCha20-Poly1305, el cálculo de hash BLAKE3 y la escritura en disco se delegan a hilos de cómputo bloqueante (`spawn_blocking`). Esto garantiza que los búferes de reensamblado de QUIC nunca se atasquen esperando I/O de disco.
2. **Ventanas de Transporte Anti-Gaps:** Prevención matemática del error interno de QUIC `too many gaps in stream buffer` manteniendo la ventana por stream estrictamente por debajo del límite de fragmentos desordenados (`MAX_CHUNKS = 1024`):
   - Ventana por stream: **1 MiB** (`DEFAULT_STREAM_RECEIVE_WINDOW`, máx. ~850 datagramas UDP).
   - Ventana de conexión acumulada: **8 MiB** (`DEFAULT_CONN_RECEIVE_WINDOW`).
   - Ventana de envío: **8 MiB** (`DEFAULT_SEND_WINDOW`).
3. **Multi-Stream Paralelo (4 Streams QUIC):** En conexiones QUIC con archivos de tamaño suficiente, la transferencia se particiona automáticamente en **4 streams bidireccionales paralelos**. Cada stream transfiere su rango asignado de bloques con encabezados independientes, distribuyendo los paquetes en la red y eliminando el bloqueo de cabeza de línea (*Head-of-Line blocking*).
4. **Control de Velocidad Dinámico en Caliente:** Regulación de ancho de banda en tiempo real mediante *token-bucket* compartido cooperativamente entre todos los streams de subida y bajada. Los límites pueden modificarse en tiempo real durante una transferencia activa desde los controles de la GUI o CLI sin interrumpir la sesión.
5. **Reconexión y Reanudación Automática (10 Intentos con Backoff Exponencial):** Ante cortes transitorios de red, saturación, microdesconexiones o fallos en bloques individuales en tránsito, el receptor reintenta conectarse automáticamente hasta 10 veces con backoff exponencial. El emisor mantiene su servidor temporal activo y la transferencia se reanuda inmediatamente desde el último bloque verificado sin perder los datos ya descargados.
6. **Diagnóstico y Trazabilidad:** Soporte nativo de `tracing` compatible con `RUST_LOG` y la bandera global `--verbose` (`-v`) en la CLI. En la GUI, los errores muestran mensajes claros en español con un desplegable de **Detalles técnicos**.

## Integridad, reanudación y archivos existentes

Los bloques se cifran con ChaCha20-Poly1305 y se verifican con BLAKE3 antes de escribirlos. El intercambio X25519 rechaza claves no contributivas. Las pruebas autenticadas cubren nombre, tamaño, hash, parámetros del archivo, decisión de recepción, offset de reanudación y confirmación final.

- **Modo rápido**, predeterminado en la GUI: omite el hash completo previo y verifica cada bloque. Reinicia un `.part` anterior, porque nombre y tamaño no identifican de forma segura su contenido.
- **Verificar archivo completo y reanudar**, en Ajustes: calcula BLAKE3 antes de generar la invitación. Permite reanudar un `.part` compatible y verifica el hash completo al terminar. La preparación de archivos muy grandes puede tardar; la validez de la invitación empieza después.
- Sin `--force`, un archivo final existente se conserva. Elige otra carpeta en la GUI. La publicación final también rechaza un archivo que aparezca durante la descarga.
- Se rechazan rutas en nombres recibidos, flujos alternativos NTFS y nombres de dispositivo Windows. Un archivo incompleto no se publica como terminado.

## CLI

```powershell
# Automático: hash previo, 4 streams QUIC y reanudación segura.
heavydrop send "F:\backups\archivo_10gb.iso"
heavydrop recv "heavy1_INVITACION_COMPLETA" --output-dir "D:\Descargas"

# Modo detallado (logs técnicos de conexión, ventanas y streams):
heavydrop --verbose send "F:\backups\archivo.iso"
heavydrop --verbose recv "heavy1_INVITACION_COMPLETA" --output-dir "D:\Descargas"

# Sin hash completo previo: inicio rápido, sin reanudar un parcial anterior.
heavydrop send "F:\backups\archivo.iso" --no-prehash

# Solo conexión directa por IP, sin relay en ambos extremos.
heavydrop send "F:\backups\archivo.iso" --direct-only
heavydrop recv "heavy1_INVITACION_COMPLETA" --direct-only --output-dir "D:\Descargas"

# Compatibilidad LAN con códigos cortos (1 stream TCP).
heavydrop send "F:\backups\archivo.iso" --lan-only
heavydrop recv KD7M-9Q2P-W8RX --legacy-tcp --output-dir "D:\Descargas"
```

## Cómo probar con un archivo de 10 GB o más

Para verificar la estabilidad y el rendimiento en archivos masivos:

1. **Generar un archivo de prueba de 10 GB en PowerShell:**
   ```powershell
   # Crea un archivo disperso de 10 GiB rápidamente
   $file = [System.IO.File]::Create("F:\archivo_10gb.bin")
   $file.SetLength(10 * 1024 * 1024 * 1024)
   $file.Close()
   ```
2. **Iniciar el emisor:**
   ```powershell
   heavydrop --verbose send "F:\archivo_10gb.bin"
   ```
   Copia la invitación generada `heavy1_...`.
3. **Iniciar el receptor:**
   ```powershell
   heavydrop --verbose recv "heavy1_..." --output-dir "D:\Descargas"
   ```
4. **Verificar en los logs:**
   - La asignación de 4 streams paralelos (`Stream 0 asignado`, `Worker stream asignado`).
   - El drenaje continuo de los bloques cifrados sin errores de *stream buffer gaps*.
   - Si desconectas la red o cancelas el receptor a mitad de la transferencia, vuelve a ejecutar el comando `recv`: HeavyDrop detectará el archivo `.part`, verificará los bloques ya descargados y reanudará automáticamente desde el último punto.

## Servidor manual de códigos cortos

Este servicio se usa en el modo TCP compatible, no es un requisito para las invitaciones QUIC:

```powershell
heavydrop server --port 7880
heavydrop send "F:\archivo.iso" --legacy-tcp --signal-server "tu-servidor.com:7880"
heavydrop recv KD7M-9Q2P-W8RX --legacy-tcp --signal-server "tu-servidor.com:7880" --output-dir "D:\Descargas"
```

## Compilar y verificar

```powershell
cargo build --release --locked
cargo test --all-targets --locked
cargo clippy --locked
```

## Releases por cada integración en main

Cada push a `main`, incluidos los merges, activa pruebas, compilación Windows x64 y publicación. Cada release incluye `heavydrop-vX.Y.Z-windows-x64.zip` y su checksum SHA-256.
