# Myna Test Reading Sample Corpus (Arabic / العربية)

Arabic (`ar`) translation of the reading sample corpus used by
`docs/test-plan-system.md`. Mirrors the structure of `docs/test-samples-en.md`
§1–§6.

> **Review status**: draft machine-assisted translation — **needs
> native/fluent Arabic speaker review before use**, per
> `docs/test-plan-system.md` §2's requirement that accuracy judgments be
> made only by a fluent/native speaker of the language being tested. This
> draft is written in Modern Standard Arabic; confirm whether a dialect
> variant should be preferred for actual testing.

**Product-specific terms convention**: product names, package names, and
technical identifiers (myna, whisper-snap, nemotron-snap, PipeWire, IBus,
Nemotron, GNOME Shell, version strings, file paths) are kept in **English**
throughout, matching how a real bilingual user would actually speak them —
these are not translated. Since Arabic is right-to-left, expect these
inline English/Latin-script tokens to be a distinct test signal for
bidirectional text handling — flag any injection issues around these
tokens specifically.

---

## 1. Natural long-form prose (2 passages)

**Passage A** (~35 seconds spoken)

> "ركّز البحث في مجال الذكاء الاصطناعي على عدد من الأهداف الرئيسية: الاستدلال،
> وتمثيل المعرفة، والتخطيط، والتعلّم، ومعالجة اللغة الطبيعية، والإدراك.
> ويُعدّ الذكاء العام، أي القدرة على إنجاز أي مهمة يمكن لإنسان أداءها، من
> ضمن الأهداف طويلة المدى لهذا المجال. ولتحقيق هذه الأهداف، استخدم الباحثون
> مجموعة واسعة من التقنيات، منها البحث والتحسين الرياضي، والمنطق الصوري،
> والشبكات العصبية الاصطناعية، وأساليب قائمة على الإحصاء والاحتمالات
> والاقتصاد."

**Passage B** (~30 seconds spoken)

> "تواجه كل مؤسسة تستخدم الحواسيب والشبكات مجموعة أساسية من مخاطر الأمن
> السيبراني. ويمكن للموظفين المساعدة في إدارة هذه المخاطر باستخدام كلمات
> مرور قوية وفريدة، والحرص على تحديث البرمجيات باستمرار، وتوخي الحذر مع
> المرفقات والروابط الواردة من مرسلين مجهولين. وتضيف المصادقة متعددة
> العوامل طبقة حماية إضافية، حتى في حال سرقة كلمة المرور. كما أن النسخ
> الاحتياطي المنتظم للملفات المهمة يعني أن هجوم برامج الفدية أو عطل
> الأجهزة لا يجب أن يؤدي إلى فقدان دائم للبيانات."

## 2. Command / short-utterance set

1. "افتح نافذة طرفية جديدة."
2. "أرسل هذا إلى الفريق قبل يوم الجمعة."
3. "فاصلة، فقرة جديدة، نقطة."
4. "تراجع عن ذلك."
5. "حدّد موعد اجتماع غدًا الساعة الثالثة بعد الظهر."
6. "رُدّ: يبدو جيدًا، أراك حينها."
7. "ابحث عن مقاهٍ قريبة."
8. "أوقف صوت الميكروفون."
9. "سطر جديد. شكرًا، أتحدث معك قريبًا."
10. "ألغِ ذلك، انسَ الأمر."

## 3. Domain / technical vocabulary passage

> "ثبّتُ حزمة myna-desktop snap إلى جانب whisper-snap و nemotron-snap، ثم
> تأكدت من أن PipeWire يوجّه الميكروفون الخاص بي بشكل صحيح. يؤدي اختصار
> لوحة المفاتيح إلى تفعيل حقن IBus، وقمت بتفعيل خيار preedit لمعاينة
> النص غير المستقر قبل تثبيته. وبعد الترقية إلى الإصدار one point three
> point zero، تحققت من الإعدادات الموجودة في tilde slash dot config
> slash myna slash settings dot json للتأكد من أن وضع streaming ما زال
> مضبوطًا على auto. ويعرض امتداد GNOME Shell مؤشر النشاط دون سرقة
> التركيز من الطرفية الخاصة بي."

## 4. Numbers, dates, and punctuation-heavy passage

> "اتصل بي على الرقم خمسة خمسة خمسة، صفر واحد أربعة اثنان، في التاسع
> والعشرين من يوليو عام ألفين وستة وعشرين. وبلغ إجمالي الفاتورة أربعمئة
> واثني عشر دولارًا وخمسين سنتًا، تستحق خلال ثلاثين يومًا. تقلع رحلتي
> الساعة السادسة وخمسة وأربعين دقيقة صباحًا من البوابة B اثني عشر، ورمز
> التأكيد هو X-Ray Tango أربعة سبعة واحد."

## 5. Pangram / phonetic smoke-test

> "نص حكيم له سر قاطع وذو شأن عظيم مكتوب على ثوب أخضر ومغلف بجلد أزرق."

*(A commonly cited Arabic pangram-style sentence, used for phonetic
density rather than as a literal translation of the English fox pangram.)*

## 6. Long continuous passage for streaming tests (30s+)

> "أعلن باحثون هذا الأسبوع أن قمرًا صناعيًا جديدًا مخصصًا للأرصاد الجوية بدأ
> بث بيانات من مداره، ما يوفر لخبراء الأرصاد صورًا بدقة أعلى مقارنة
> بالأجيال السابقة من الأجهزة. ويحمل القمر الصناعي، الذي أُطلق في وقت
> سابق من هذا العام، مستشعرات قادرة على تتبع الأنظمة العاصفية بشكل شبه
> فوري، ما يقول المسؤولون إنه سيُحسّن الإنذار المبكر للمجتمعات الساحلية.
> وفي الوقت ذاته، أكد مهندسو مركز التحكم بالمهمة أن جميع الأنظمة على متن
> المركبة تعمل ضمن المعايير المتوقعة، وأن المركبة الفضائية أتمت بنجاح
> أولى مناوراتها لتعديل مدارها. ومن المتوقع أن تكتمل المحطة الرئيسية
> التالية، وهي معايرة كاملة لأجهزة التصوير، خلال الشهر المقبل، وبعدها
> سيبدأ القمر الصناعي خدمته التشغيلية الاعتيادية."
