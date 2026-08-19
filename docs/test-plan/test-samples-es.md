# Myna Test Reading Sample Corpus (Spanish / Español)

Spanish (`es`) translation of the reading sample corpus used by
`docs/test-plan-system.md`. Mirrors the structure of `docs/test-samples-en.md`
§1–§6.

> **Review status**: draft machine-assisted translation — **needs
> native/fluent Spanish speaker review before use**, per
> `docs/test-plan-system.md` §2's requirement that accuracy judgments be
> made only by a fluent/native speaker of the language being tested.

**Product-specific terms convention**: product names, package names, and
technical identifiers (myna, whisper-snap, nemotron-snap, PipeWire, IBus,
Nemotron, GNOME Shell, version strings, file paths) are kept in **English**
throughout, matching how a real bilingual user would actually speak them —
these are not translated.

---

## 1. Natural long-form prose (2 passages)

**Passage A** (~35 seconds spoken)

> "La investigación en inteligencia artificial se ha centrado en unos pocos
> objetivos clave: el razonamiento, la representación del conocimiento, la
> planificación, el aprendizaje, el procesamiento del lenguaje natural y la
> percepción. La inteligencia general, la capacidad de realizar cualquier
> tarea que un ser humano pueda desempeñar, se encuentra entre los objetivos
> a largo plazo del campo. Para alcanzar estas metas, los investigadores han
> utilizado una amplia gama de técnicas, entre ellas la búsqueda y la
> optimización matemática, la lógica formal, las redes neuronales
> artificiales, y métodos basados en la estadística, la probabilidad y la
> economía."

**Passage B** (~30 seconds spoken)

> "Toda organización que utiliza computadoras y redes enfrenta un conjunto
> básico de riesgos de ciberseguridad. Los empleados pueden ayudar a
> gestionar esos riesgos usando contraseñas seguras y únicas, manteniendo el
> software actualizado, y teniendo cuidado con los archivos adjuntos y
> enlaces de remitentes desconocidos. La autenticación multifactor añade una
> capa adicional de protección, incluso si una contraseña es robada. Hacer
> copias de seguridad de los archivos importantes con regularidad significa
> que un ataque de ransomware o una falla de hardware no tienen por qué
> implicar la pérdida permanente de datos."

## 2. Command / short-utterance set

1. "Abre una nueva ventana de terminal."
2. "Envía esto al equipo antes del viernes."
3. "Coma, nuevo párrafo, punto."
4. "Deshaz eso."
5. "Programa una reunión para mañana a las tres de la tarde."
6. "Responde: suena bien, nos vemos entonces."
7. "Busca cafeterías cercanas."
8. "Silencia el micrófono."
9. "Nueva línea. Gracias, hablamos pronto."
10. "Cancela eso, olvídalo."

## 3. Domain / technical vocabulary passage

> "Instalé el snap de myna-desktop junto con whisper-snap y nemotron-snap,
> y luego confirmé que PipeWire estaba enrutando correctamente mi
> micrófono. La combinación de teclas activa la inyección de IBus, y
> habilité la opción de preedit para previsualizar el texto inestable antes
> de que se confirme. Después de actualizar a la versión one point three
> point zero, revisé la configuración en tilde slash dot config slash myna
> slash settings dot json para asegurarme de que el modo streaming
> siguiera configurado en auto. La extensión de GNOME Shell muestra el
> indicador de actividad sin quitarle el foco a mi terminal."

## 4. Numbers, dates, and punctuation-heavy passage

> "Llámame al cinco cinco cinco, cero uno cuatro dos, el veintinueve de
> julio de dos mil veintiséis. El total de la factura fue de cuatrocientos
> doce dólares con cincuenta centavos, con vencimiento a treinta días. Mi
> vuelo sale a las seis y cuarenta y cinco de la mañana desde la puerta B
> doce, y el código de confirmación es equis, ere, tango, cuatro, siete,
> uno."

## 5. Pangram / phonetic smoke-test

> "El veloz murciélago hindú comía feliz cardillo y kiwi. La cigüeña
> tocaba el saxofón detrás del palenque de paja."

*(A standard Spanish pangram pair, used for phonetic density rather than as
a literal translation of the English fox pangram.)*

## 6. Long continuous passage for streaming tests (30s+)

> "Los investigadores anunciaron esta semana que un nuevo satélite
> meteorológico ha comenzado a transmitir datos desde la órbita,
> proporcionando a los meteorólogos imágenes de mayor resolución que las
> generaciones anteriores de instrumentos. El satélite, lanzado a
> principios de este año, lleva sensores capaces de rastrear sistemas de
> tormentas casi en tiempo real, lo que según los funcionarios debería
> mejorar las alertas tempranas para las comunidades costeras. Mientras
> tanto, los ingenieros del centro de control de la misión confirmaron que
> todos los sistemas a bordo están funcionando dentro de los parámetros
> esperados, y que la nave ha completado con éxito su primera maniobra de
> ajuste orbital. El próximo hito importante, una calibración completa de
> los instrumentos de imagen, se espera que se complete dentro del próximo
> mes, tras lo cual el satélite comenzará su servicio operativo de
> rutina."
