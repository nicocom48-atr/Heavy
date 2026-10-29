# ⚡ HeavyDrop: Transferencia P2P de Archivos Masivos (50GB+) en Rust

**HeavyDrop** es una herramienta en Rust diseñada específicamente para la transferencia punto a punto (P2P) de archivos de cualquier tamaño (50 GB, 100 GB, 1 TB o más) con máxima velocidad, cifrado de transporte AEAD de extremo a extremo, verificación estricta de integridad y reanudación inteligente.

Cuenta con una **interfaz gráfica nativa de escritorio (GUI)** moderna y minimalista, además de una interfaz de línea de comandos (CLI) completa para terminales y automatizaciones.

> **Nota de Distribución Oficial:** Las versiones binarias oficiales precompiladas de HeavyDrop se distribuyen **exclusivamente para Windows x64**. El código base está escrito en Rust multiplataforma para facilitar su compilación local en otros entornos si se desea.

---

## 🖥️ Interfaz Gráfica Nativa (GUI)

Al abrir `heavydrop.exe` sin argumentos (o haciendo doble clic), se inicia la aplicación de escritorio simplificada:

- 📤 **Pestaña Enviar:**
  - **Arrastrar y soltar (Drag and Drop):** Arrastra cualquier archivo directamente a la ventana para prepararlo al instante.
  - **Descubrimiento Local Automático:** Emite un beacon en la red local (LAN) para que otros equipos te detecten sin necesidad de copiar IPs manualmente.
  - **Opciones Avanzadas Plegables:** Configura puerto TCP, tamaño de chunks (MB), token de acceso o prehash únicamente si lo necesitas.
- 📥 **Pestaña Recibir:**
  - **Detección de Equipos en Red:** Lista automática de emisores activos en tu red local con botón de conexión en 1 clic.
  - **Conexión Manual:** Campo directo para ingresar la IP o dirección del emisor.
- 📊 **Monitor Unificado de Transferencia:**
  - Muestra dispositivo/peer, nombre de archivo, barra de progreso porcentual, velocidad en tiempo real (MB/s) y tiempo restante estimado (ETA).
  - **Cancelación Cooperativa:** Botón de cancelación limpia que detiene la transferencia de inmediato sin dejar archivos inconsistentes.

---

## 🔒 Arquitectura y Seguridad

1. **Cifrado de Transporte AEAD (ChaCha20-Poly1305 + X25519):**
   - Cada sesión de transferencia negocia un secreto compartido efímero mediante un intercambio de claves **X25519** (Perfect Forward Secrecy).
   - Todos los chunks y tramas de datos viajan cifrados y autenticados con **ChaCha20-Poly1305 AEAD**. Cualquier manipulación en tránsito es detectada y rechazada de inmediato.
2. **Autenticación Criptográfica de Conocimiento Cero (Challenge-Response):**
   - El token secreto nunca se envía en texto claro por la red.
   - El receptor genera una prueba BLAKE3 basada en un desafío (*challenge*) del emisor y el token compartido; el emisor valida la prueba en tiempo constante (`constant_time_eq`) para prevenir ataques de temporización.
3. **Reanudación Segura con Archivos `.part` y Metadatos:**
   - Las transferencias incompletas se almacenan como `archivo.part` acompañadas de `archivo.part.meta`.
   - Si la red se corta tras 40 GB de un archivo de 50 GB, el receptor verifica el último fragmento local guardado. Si coincide, reanuda exactamente desde ese punto. Si hubo corrupción en disco, descarta el tramo inválido automáticamente.
   - Una vez alcanzado el 100% y validado el hash global, el archivo `.part` se renombra atómicamente a su nombre final.
4. **Protección Estricta contra Path Traversal:**
   - Sanitización rigurosa de nombres de archivo contra rutas relativas (`../`), separadores de directorio y nombres de dispositivos reservados de Windows (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`).
5. **Memoria RAM Constante (< 20 MB):**
   - Streaming por bloques (*chunks* de 4 MB) de flujo acotado. Nunca se carga el archivo entero en memoria.
6. **Descubrimiento Local sin la Nube:**
   - Baliza UDP ligera en red local sin servidores centrales, telemetría ni intermediarios.

---

## 📦 Descargas Oficiales (Windows x64)

Los binarios oficiales se publican automáticamente en la sección de **[Releases en GitHub](https://github.com/nicocom48-atr/Heavy/releases)**:

- 🪟 **Windows x64:** `heavydrop-vX.Y.Z-windows-x64.zip` (incluye `heavydrop.exe` optimizado y su checksum `heavydrop-vX.Y.Z-windows-x64.zip.sha256`).

Para verificar la integridad del archivo descargado en PowerShell:
```powershell
Get-FileHash -Algorithm SHA256 heavydrop-vX.Y.Z-windows-x64.zip
```

---

## 📖 Uso por Línea de Comandos (CLI)

### 1. Enviar un archivo

```bash
heavydrop send "F:\backups\archivo_50gb.iso"
```

Opciones principales:
- `--token <PASSWORD>`: Exige autenticación criptográfica al receptor.
- `--port <PORT>`: Especifica el puerto TCP (por defecto `7878`).
- `--chunk-size-mb <MB>`: Tamaño de bloque en MB (por defecto `4`).
- `--no-prehash`: Comienza la transferencia de inmediato sin esperar el hash previo de archivos gigantes.

### 2. Recibir un archivo

```bash
heavydrop recv 192.168.1.45:7878
```

Opciones principales:
- `--output-dir <DIR>`: Carpeta donde guardar el archivo (por defecto directorio actual).
- `--token <PASSWORD>`: Clave de autenticación si el emisor la requirió.
- `--force`: Sobrescribe archivos existentes sin preguntar.

---

## 🛠️ Compilación desde el Código Fuente

Si deseas compilar HeavyDrop desde el código fuente con Rust:

1. Clona el repositorio:
   ```bash
   git clone https://github.com/nicocom48-atr/Heavy.git
   cd Heavy
   ```
2. Compila en modo Release:
   ```bash
   cargo build --release
   ```
3. El ejecutable se genera en:
   - `target/release/heavydrop.exe` (Windows)

Para ejecutar la suite completa de pruebas:
```bash
cargo test
```
