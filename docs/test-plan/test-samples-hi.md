# Myna Test Reading Sample Corpus (Hindi / हिन्दी)

Hindi (`hi`) translation of the reading sample corpus used by
`docs/test-plan-system.md`. Mirrors the structure of `docs/test-samples-en.md`
§1–§6.

> **Review status**: draft machine-assisted translation — **needs
> native/fluent Hindi speaker review before use**, per
> `docs/test-plan-system.md` §2's requirement that accuracy judgments be
> made only by a fluent/native speaker of the language being tested.

**Product-specific terms convention**: product names, package names, and
technical identifiers (myna, whisper-snap, nemotron-snap, PipeWire, IBus,
Nemotron, GNOME Shell, version strings, file paths) are kept in **English**
throughout, matching how a real bilingual user would actually speak them —
these are not translated. Expect frequent code-switching between
Devanagari and Latin script for these tokens — this is itself a relevant
test signal for script-mixing handling, not an error to normalize away.

---

## 1. Natural long-form prose (2 passages)

**Passage A** (~35 seconds spoken)

> "कृत्रिम बुद्धिमत्ता के शोध ने कुछ प्रमुख लक्ष्यों पर ध्यान केंद्रित किया है:
> तर्क, ज्ञान का प्रतिनिधित्व, योजना बनाना, सीखना, प्राकृतिक भाषा प्रसंस्करण,
> और अनुभूति। सामान्य बुद्धिमत्ता, यानी किसी भी ऐसे कार्य को पूरा करने की
> क्षमता जो एक इंसान कर सकता है, इस क्षेत्र के दीर्घकालिक लक्ष्यों में से एक
> है। इन लक्ष्यों तक पहुँचने के लिए, शोधकर्ताओं ने तकनीकों की एक विस्तृत
> श्रृंखला का उपयोग किया है, जिसमें खोज और गणितीय अनुकूलन, औपचारिक तर्कशास्त्र,
> कृत्रिम तंत्रिका नेटवर्क, और सांख्यिकी, प्रायिकता तथा अर्थशास्त्र पर आधारित
> विधियाँ शामिल हैं।"

**Passage B** (~30 seconds spoken)

> "हर वह संगठन जो कंप्यूटर और नेटवर्क का उपयोग करता है, उसे साइबर सुरक्षा से
> जुड़े कुछ बुनियादी जोखिमों का सामना करना पड़ता है। कर्मचारी मजबूत और
> अद्वितीय पासवर्ड का उपयोग करके, सॉफ्टवेयर को अद्यतन रखकर, और अज्ञात
> प्रेषकों से आए अटैचमेंट तथा लिंक से सावधान रहकर इन जोखिमों को प्रबंधित करने
> में मदद कर सकते हैं। मल्टी-फैक्टर प्रमाणीकरण सुरक्षा की एक अतिरिक्त परत
> जोड़ता है, भले ही पासवर्ड चोरी हो जाए। महत्वपूर्ण फ़ाइलों का नियमित रूप से
> बैकअप लेने का मतलब है कि रैनसमवेयर हमला या हार्डवेयर की खराबी स्थायी
> डेटा हानि का कारण न बने।"

## 2. Command / short-utterance set

1. "एक नई टर्मिनल विंडो खोलो।"
2. "इसे शुक्रवार तक टीम को भेज दो।"
3. "कॉमा, नया पैराग्राफ, पूर्ण विराम।"
4. "उसे पूर्ववत करो।"
5. "कल दोपहर तीन बजे के लिए एक मीटिंग शेड्यूल करो।"
6. "जवाब दो: ठीक है, फिर मिलते हैं।"
7. "आस-पास की कॉफ़ी शॉप खोजो।"
8. "माइक्रोफ़ोन म्यूट करो।"
9. "नई लाइन। धन्यवाद, जल्द बात होगी।"
10. "उसे रद्द करो, कोई बात नहीं।"

## 3. Domain / technical vocabulary passage

> "मैंने myna-desktop snap को whisper-snap और nemotron-snap के साथ
> इंस्टॉल किया, फिर पुष्टि की कि PipeWire मेरे माइक्रोफ़ोन को सही तरीके से
> रूट कर रहा था। हॉटकी IBus इंजेक्शन को ट्रिगर करती है, और मैंने कमिट होने
> से पहले अस्थिर टेक्स्ट को प्रीव्यू करने के लिए preedit फ्लैग सक्षम किया।
> version one point three point zero में अपग्रेड करने के बाद, मैंने
> tilde slash dot config slash myna slash settings dot json पर कॉन्फ़िग
> चेक की ताकि यह सुनिश्चित हो सके कि streaming मोड अभी भी auto पर सेट है।
> GNOME Shell एक्सटेंशन मेरे टर्मिनल से फोकस चुराए बिना एक्टिविटी इंडिकेटर
> दिखाता है।"

## 4. Numbers, dates, and punctuation-heavy passage

> "मुझे पाँच पाँच पाँच, शून्य एक चार दो पर, उनतीस जुलाई दो हज़ार छब्बीस को
> कॉल करो। इनवॉइस की कुल राशि चार सौ बारह डॉलर और पचास सेंट थी, जो तीस
> दिनों के भीतर देय है। मेरी फ़्लाइट सुबह छह बजकर पैंतालीस मिनट पर गेट बी
> बारह से रवाना होती है, और कन्फ़र्मेशन कोड X-Ray Tango चार सात एक है।"

## 5. Pangram / phonetic smoke-test

> "ऋषियों को सताने वाले दुष्ट राक्षस ने पाँच अच्छे योद्धाओं को हराया।"

*(A constructed Hindi sentence chosen for broad phonetic coverage, used for
phonetic density rather than as a literal translation of the English fox
pangram — Hindi does not have a single, widely standardized pangram the
way English or Czech do.)*

## 6. Long continuous passage for streaming tests (30s+)

> "शोधकर्ताओं ने इस सप्ताह घोषणा की कि एक नए मौसम उपग्रह ने कक्षा से डेटा भेजना
> शुरू कर दिया है, जो मौसम विज्ञानियों को उपकरणों की पिछली पीढ़ियों की तुलना
> में अधिक उच्च-रिज़ॉल्यूशन वाली तस्वीरें उपलब्ध करा रहा है। इस साल की
> शुरुआत में लॉन्च किया गया यह उपग्रह, तूफान प्रणालियों को लगभग वास्तविक
> समय में ट्रैक करने में सक्षम सेंसर ले जाता है, जिससे अधिकारियों के अनुसार
> तटीय समुदायों के लिए शुरुआती चेतावनियाँ बेहतर होनी चाहिए। इसी बीच, मिशन के
> ग्राउंड कंट्रोल सेंटर के इंजीनियरों ने पुष्टि की कि सभी ऑनबोर्ड सिस्टम
> अपेक्षित सीमाओं के भीतर काम कर रहे हैं, और अंतरिक्ष यान ने अपना पहला
> कक्षा-समायोजन युद्धाभ्यास सफलतापूर्वक पूरा कर लिया है। अगला बड़ा पड़ाव,
> इमेजिंग उपकरणों का पूर्ण कैलिब्रेशन, आने वाले महीने के भीतर पूरा होने की
> उम्मीद है, जिसके बाद उपग्रह अपनी नियमित परिचालन सेवा शुरू करेगा।"
