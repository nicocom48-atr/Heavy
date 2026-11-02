use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "heavydrop",
    author = "HeavyDrop Contributors",
    version,
    about = "⚡ Transferencia de archivos P2P masivos (50GB+) mediante códigos temporales únicos sin configuración de IPs."
)]
pub struct Cli {
    /// Activar registro detallado de eventos de red y depuración técnica
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Enviar un archivo generando un código temporal de conexión
    Send {
        /// Usar código corto y descubrimiento TCP local (compatibilidad)
        #[arg(long, conflicts_with = "direct_only")]
        lan_only: bool,
        /// Solo P2P directo por QUIC, sin relays ni servicios públicos
        #[arg(long, conflicts_with_all = ["lan_only", "legacy_tcp"])]
        direct_only: bool,
        /// Usar códigos cortos y señalización TCP en lugar de invitaciones QUIC
        #[arg(long, conflicts_with = "direct_only")]
        legacy_tcp: bool,
        /// Ruta al archivo a transferir
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Omitir el hash previo del archivo completo (los bloques se siguen verificando en tránsito)
        #[arg(long)]
        no_prehash: bool,

        /// Avanzado: Puerto TCP preferido para conexiones entrantes (por defecto: 7878)
        #[arg(long, default_value_t = 7878)]
        port: u16,

        /// Avanzado: Servidor de señalización personalizado (host:puerto)
        #[arg(long)]
        signal_server: Option<String>,
    },

    /// Recibir un archivo utilizando el código temporal compartido por el emisor
    Recv {
        /// Permitir códigos cortos TCP: el handshake expone metadatos al relay
        #[arg(long, conflicts_with = "direct_only")]
        legacy_tcp: bool,
        /// Rechazar relays: conectar únicamente por una dirección IP directa
        #[arg(long)]
        direct_only: bool,
        /// Invitación completa heavy1_… (los códigos cortos requieren --legacy-tcp)
        #[arg(value_name = "CODE")]
        code: String,

        /// Directorio donde se guardará el archivo recibido (por defecto: carpeta actual)
        #[arg(short, long, default_value = ".")]
        output_dir: PathBuf,

        /// Sobrescribir archivo existente si ya existe
        #[arg(short, long)]
        force: bool,

        /// Avanzado: Servidor de señalización personalizado (host:puerto)
        #[arg(long)]
        signal_server: Option<String>,
    },

    /// Iniciar la interfaz gráfica interactiva (por defecto si no se pasa comando)
    Gui,

    /// Ejecutar el servidor ligero de señalización y relay (rendezvous)
    Server {
        /// Puerto TCP para escuchar conexiones de señalización y relay (por defecto: 7880)
        #[arg(short, long, default_value_t = 7880)]
        port: u16,
    },
}
