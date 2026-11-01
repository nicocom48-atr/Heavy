# HeavyDrop

Transferencia de archivos grandes entre equipos, con interfaz nativa para Windows y CLI en Rust. El transporte predeterminado usa QUIC mediante Iroh, busca una ruta P2P directa y mantiene un relay cifrado como respaldo cuando el NAT o el firewall impiden esa ruta.

Las distribuciones oficiales son para **Windows x64**. Descargas: [Releases](https://github.com/nicocom48-atr/Heavy/releases).

## Conectar dos equipos

1. Selecciona o arrastra el archivo en **Enviar archivo** y pulsa **Crear transferencia**.
2. Copia la **invitación completa `heavy1_…`** y compártela con el receptor. Contiene la identidad y las rutas del emisor; no necesita un servidor manual de códigos cortos.
3. El receptor pega la invitación en **Código del emisor**, elige una carpeta y pulsa **Conectar**.
4. La invitación tiene cinco minutos de validez y es de un solo uso. El listener temporal se cierra al terminar, cancelar o expirar.

El diseño **Noche / Cian** integra Enviar, Recibir, Historial, Velocidad y Ajustes en una pantalla adaptable. La ventana inicial mide 1280 × 1040 y conserva desplazamiento en pantallas pequeñas. La GUI se abre sin consola adicional, también en desarrollo.

## ¿Seguimos siendo P2P?

Sí. El modo automático usa direcciones IP directas y perforación de NAT cuando es posible. Iroh puede iniciar la conexión por relay y pasar a una ruta directa; el monitor muestra la ruta seleccionada mientras se transfiere. El relay reenvía tráfico cifrado: no almacena el archivo como un servicio de nube.

- **Automático, recomendado:** permite conexión directa LAN/Internet y relay como respaldo. No exige que un relay esté disponible para habilitar una transferencia local. Si no estuvo disponible durante la preparación, la invitación puede contener únicamente rutas directas.
- **Solo P2P directo (sin relay):** desactiva relays tanto al enviar como al recibir. Puede fallar entre redes con NAT/CGNAT o firewalls restrictivos. No es una forma de atravesar cualquier NAT sin infraestructura auxiliar.
- **Códigos cortos (TCP compatible):** modo explícito de compatibilidad. Usa descubrimiento LAN y, opcionalmente, el servidor manual de señalización/relay. Completar ese servidor no cambia silenciosamente el transporte automático a TCP.

Las invitaciones usan ALPN `heavydrop/5` y protocolo v5. **Actualiza ambos equipos**: no son compatibles con clientes de protocolo v4.

## Integridad, reanudación y archivos existentes

Los bloques se cifran con ChaCha20-Poly1305 y se verifican con BLAKE3 antes de escribirlos. El intercambio X25519 rechaza claves no contributivas. Las pruebas autenticadas cubren nombre, tamaño, hash, parámetros del archivo, decisión de recepción, offset de reanudación y confirmación final.

- **Modo rápido**, predeterminado en la GUI: omite el hash completo previo y verifica cada bloque. Reinicia un `.part` anterior, porque nombre y tamaño no identifican de forma segura su contenido.
- **Verificar archivo completo y reanudar**, en Ajustes: calcula BLAKE3 antes de generar la invitación. Permite reanudar un `.part` compatible y verifica el hash completo al terminar. La preparación de archivos muy grandes puede tardar; la validez de la invitación empieza después.
- Sin `--force`, un archivo final existente se conserva. Elige otra carpeta en la GUI. La publicación final también rechaza un archivo que aparezca durante la descarga.
- Se rechazan rutas en nombres recibidos, flujos alternativos NTFS y nombres de dispositivo Windows. Un archivo incompleto no se publica como terminado.

La memoria está acotada por bloques, colas y ventanas QUIC, no por el tamaño total del archivo. Las ventanas predeterminadas son 16 MiB por stream y 32 MiB por conexión/envío; la GUI y las bibliotecas añaden consumo. No se promete un total inferior a 20 MB.

La invitación es una credencial: quien la posee puede intentar recibir el archivo. Este protocolo utiliza autenticación simétrica; no es un PAKE ni una prueba de conocimiento cero. En el modo TCP compatible el relay puede observar los metadatos del handshake, aunque los bloques del archivo estén cifrados. En QUIC, el handshake de aplicación también viaja cifrado.

## CLI

```powershell
# Automático: hash previo, invitación QUIC y reanudación segura.
heavydrop send "F:\backups\archivo.iso"
heavydrop recv "heavy1_INVITACION_COMPLETA" --output-dir "D:\Descargas"

# Sin hash completo previo: inicio rápido, sin reanudar un parcial anterior.
heavydrop send "F:\backups\archivo.iso" --no-prehash

# Solo conexión directa por IP, sin relay en ambos extremos.
heavydrop send "F:\backups\archivo.iso" --direct-only
heavydrop recv "heavy1_INVITACION_COMPLETA" --direct-only --output-dir "D:\Descargas"

# Compatibilidad LAN con códigos cortos.
heavydrop send "F:\backups\archivo.iso" --lan-only
heavydrop recv KD7M-9Q2P-W8RX --output-dir "D:\Descargas"
```

`--force` en `recv` permite reemplazar el destino e ignora parciales existentes. `--help` muestra las opciones completas.

## Servidor manual de códigos cortos

Este servicio se usa en el modo TCP compatible, no es un requisito para las invitaciones QUIC:

```powershell
heavydrop server --port 7880
heavydrop send "F:\archivo.iso" --legacy-tcp --signal-server "tu-servidor.com:7880"
heavydrop recv KD7M-9Q2P-W8RX --signal-server "tu-servidor.com:7880" --output-dir "D:\Descargas"
```

También se puede configurar `HEAVYDROP_SIGNAL_SERVER` o **Servidor de Internet** en Ajustes. En la GUI se debe activar **Códigos cortos (TCP compatible)** para enviar con ese transporte. El servidor necesita TCP entrante en el puerto elegido. En LAN, el modo compatible usa UDP 7879 para descubrimiento y TCP para datos; QUIC utiliza UDP.

## Compilar y verificar

```powershell
cargo build --release --locked
cargo test --all-targets --locked
pwsh -ExecutionPolicy Bypass -File tests/release_workflow_tests.ps1
cargo audit
```

Las pruebas de integración usan archivos generados y conexiones locales. La prueba de infraestructura pública es opcional y está omitida por defecto. No sustituyen una prueba WAN entre equipos en redes distintas ni una auditoría criptográfica independiente. Ver [revisión de seguridad y rendimiento](docs/SECURITY_REVIEW.md).

## Releases por cada integración en main

Cada push a `main`, incluidos los merges, activa pruebas, compilación Windows x64 y publicación. La versión patch se calcula a partir de las etiquetas existentes. La versión de `Cargo.toml` y `Cargo.lock` se ajusta en un commit de release al que apunta la etiqueta; el bot no modifica `main`.

Las publicaciones se encolan, hasta el límite de GitHub de 100 pendientes. Reintentar una ejecución reutiliza su versión y reconstruye/publica sus archivos, incluso si la etiqueta ya se había subido. No se crea una versión adicional por ese reintento.

Cada release incluye `heavydrop-vX.Y.Z-windows-x64.zip` y su checksum SHA-256:

```powershell
Get-FileHash -Algorithm SHA256 heavydrop-vX.Y.Z-windows-x64.zip
```

`index.html` se conserva como referencia visual interactiva; la aplicación de escritorio está implementada en egui/eframe.
